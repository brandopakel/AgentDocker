//! Projects, attention and contextual tools over live daemon state.
//!
//! The window is a rail and a workspace. The rail carries the mark, the
//! four destinations and the project list; the workspace leads with the
//! project name, then the section tabs, then whatever the section shows.
//! Status is drawn as a coloured dot *and* said in words, every time.
use super::icons::{Icon, icon};
use super::style::{Colors, alpha, weight};
use super::*;
use crate::controls::{
    Kind, block_button, button as action, custom, danger, input, input_enabled, primary, segment,
    tab,
};
use iced::{
    Center, Element, Fill, Font,
    widget::{Space, column, container, row, scrollable, text},
};

fn heading<'a>(value: impl Into<String>, size: u32) -> iced::widget::Text<'a> {
    text(value.into())
        .size(size)
        .font(weight(iced::font::Weight::Semibold))
}
fn title<'a>(value: impl Into<String>, size: u32) -> iced::widget::Text<'a> {
    text(value.into())
        .size(size)
        .font(weight(iced::font::Weight::Semibold))
}
fn note<'a>(value: impl Into<String>, c: Colors) -> iced::widget::Text<'a> {
    text(value.into()).size(13).color(c.muted)
}
fn small<'a>(value: impl Into<String>, c: Colors) -> iced::widget::Text<'a> {
    text(value.into()).size(12).color(c.muted)
}
/// A section label: short, quiet, set in capitals.
fn eyebrow<'a>(value: impl Into<String>, c: Colors) -> iced::widget::Text<'a> {
    text(value.into().to_uppercase())
        .size(11)
        .color(c.faint)
        .font(weight(iced::font::Weight::Semibold))
}
fn mono<'a>(value: impl Into<String>, c: Colors) -> iced::widget::Text<'a> {
    text(value.into())
        .size(12)
        .font(Font::MONOSPACE)
        .color(c.muted)
}
fn card<'a>(content: impl Into<Element<'a, Message>>, c: Colors) -> Element<'a, Message> {
    container(content)
        .padding(18)
        .width(Fill)
        .style(move |_| c.card_style())
        .into()
}
/// A card that holds rows rather than prose: tighter padding.
fn panel<'a>(content: impl Into<Element<'a, Message>>, c: Colors) -> Element<'a, Message> {
    container(content)
        .padding(6)
        .width(Fill)
        .style(move |_| c.card_style())
        .into()
}
fn attention<'a>(
    content: impl Into<Element<'a, Message>>,
    tint: iced::Color,
    c: Colors,
) -> Element<'a, Message> {
    container(content)
        .padding(18)
        .width(Fill)
        .style(move |_| c.attention_style(tint))
        .into()
}
fn pill<'a>(
    label: impl Into<String>,
    background: iced::Color,
    ink: iced::Color,
    c: Colors,
) -> Element<'a, Message> {
    container(
        text(label.into())
            .size(12)
            .font(weight(iced::font::Weight::Semibold)),
    )
    .padding([3, 9])
    .style(move |_| c.pill(background, ink))
    .into()
}
fn dot<'a>(fill: iced::Color, size: f32, c: Colors) -> Element<'a, Message> {
    container(Space::new().width(size).height(size))
        .style(move |_| c.dot(fill))
        .into()
}
/// A project's mark: its initial on its own tint. The tint comes from
/// the project's identity, so a repository keeps its colour across
/// clones, machines and themes, and two projects side by side are told
/// apart before their names are read.
fn monogram<'a>(name: &str, seed: &str, size: f32, c: Colors) -> Element<'a, Message> {
    let (tint, ink) = super::style::identity(seed, c.dark);
    let initial: String = name
        .chars()
        .find(|ch| ch.is_alphanumeric())
        .map(|ch| ch.to_uppercase().collect())
        .unwrap_or_else(|| "·".to_owned());
    container(
        text(initial)
            .size(size * 0.55)
            .font(weight(iced::font::Weight::Semibold))
            .color(ink),
    )
    .center(size)
    .style(move |_| container::Style {
        background: Some(tint.into()),
        border: iced::Border {
            radius: (size * 0.28).into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}
/// Something small that says what it means when pointed at. Used only
/// where the words are not already printed beside it (the rail's project
/// marks); a tooltip that repeats visible text is noise.
fn hint<'a>(
    content: impl Into<Element<'a, Message>>,
    label: impl Into<String>,
    c: Colors,
) -> Element<'a, Message> {
    // The mark is small; the thing you point at is not. Padding widens
    // the hover target to a comfortable size without moving the mark.
    iced::widget::tooltip(
        container(content).padding(5),
        container(text(label.into()).size(12).color(c.text))
            .padding([5, 9])
            .style(move |_| container::Style {
                shadow: iced::Shadow {
                    color: iced::Color::from_rgba8(16, 24, 40, 0.18),
                    offset: iced::Vector::new(0.0, 2.0),
                    blur_radius: 6.0,
                },
                ..c.surface(c.card, true)
            }),
        iced::widget::tooltip::Position::Right,
    )
    .gap(8)
    .into()
}
/// A track holding `segment` choices.
fn segmented<'a>(choices: Vec<Element<'a, Message>>, c: Colors) -> Element<'a, Message> {
    let mut track = row![].spacing(2);
    for choice in choices {
        track = track.push(choice);
    }
    container(track)
        .padding(3)
        .style(move |_| container::Style {
            background: Some(c.raised.into()),
            border: iced::Border {
                radius: 10.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}
/// How much of something is left, as a thin bar.
fn meter<'a>(fraction: f32, fill: iced::Color, c: Colors) -> Element<'a, Message> {
    let left = (fraction.clamp(0.0, 1.0) * 1000.0).round() as u16;
    let mut bar = row![].spacing(0);
    if left > 0 {
        bar = bar.push(
            container(Space::new().width(Fill).height(4))
                .width(iced::Length::FillPortion(left))
                .style(move |_| c.dot(fill)),
        );
    }
    if left < 1000 {
        bar = bar.push(Space::new().width(iced::Length::FillPortion(1000 - left)));
    }
    container(bar)
        .width(Fill)
        .style(move |_| c.dot(c.line))
        .into()
}
/// The strip along the top of a pane: what it holds, and one quiet fact.
fn pane_header<'a>(
    title_text: &'a str,
    meta: impl Into<String>,
    c: Colors,
) -> Element<'a, Message> {
    column![
        container(
            row![eyebrow(title_text, c).width(Fill), small(meta, c)]
                .spacing(10)
                .align_y(Center),
        )
        .padding([8, 12]),
        rule(c)
    ]
    .into()
}
fn rule<'a>(c: Colors) -> Element<'a, Message> {
    container(Space::new().width(Fill).height(1))
        .style(move |_| c.rule())
        .into()
}
fn kv<'a>(label: &'a str, value: impl Into<String>, c: Colors) -> Element<'a, Message> {
    row![
        small(label, c).width(110),
        text(value.into()).size(13).width(Fill)
    ]
    .spacing(10)
    .into()
}
fn value(json: &serde_json::Value, key: &str) -> String {
    json[key].as_str().unwrap_or("unknown").to_owned()
}
/// How much of a window from `start` to `end` is still ahead of `now`, as
/// a fraction from 0 (over) to 1 (not begun). A window of no length is over.
fn remaining_fraction(
    start: chrono::DateTime<Utc>,
    end: chrono::DateTime<Utc>,
    now: chrono::DateTime<Utc>,
) -> f32 {
    let total = (end - start).num_milliseconds();
    if total <= 0 {
        return 0.0;
    }
    let left = (end - now).num_milliseconds();
    (left as f64 / total as f64).clamp(0.0, 1.0) as f32
}
/// The first non-empty line of a text, cut to `limit` characters.
fn first_line(text: &str, limit: usize) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let mut out: String = line.chars().take(limit).collect();
    if line.chars().count() > limit {
        out.push('…');
    }
    out
}
/// What a message says. Agents and the CLI send `{"text": ...}`, channel
/// notices add a title and room around it; only a payload with no text at
/// all is shown as its JSON.
fn spoken_payload(payload: &serde_json::Value) -> String {
    payload
        .as_str()
        .or_else(|| payload["text"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| serde_json::to_string_pretty(payload).unwrap_or_default())
}

/// The mark, decoded once. The PNG is the cube alone on transparency,
/// downscaled for a 30-point slot at two-times density.
fn mark() -> iced::widget::image::Handle {
    static MARK: std::sync::OnceLock<iced::widget::image::Handle> = std::sync::OnceLock::new();
    MARK.get_or_init(|| {
        let mut reader = png::Decoder::new(std::io::Cursor::new(include_bytes!("../mark.png")))
            .read_info()
            .expect("embedded mark");
        let mut rgba = vec![0; reader.output_buffer_size().expect("mark buffer")];
        let info = reader.next_frame(&mut rgba).expect("embedded PNG");
        rgba.truncate(info.buffer_size());
        iced::widget::image::Handle::from_rgba(info.width, info.height, rgba)
    })
    .clone()
}
/// The mark and the two-tone wordmark. Two plain texts rather than one
/// rich text: the rich text widget resolves heavier faces differently and
/// lands on a monospace fallback for the system sans.
fn brand<'a>(c: Colors) -> Element<'a, Message> {
    row![
        iced::widget::image(mark()).width(30).height(30),
        row![
            heading("Agent", 19).color(c.text),
            heading("Docker", 19).color(c.accent)
        ]
    ]
    .spacing(10)
    .align_y(Center)
    .into()
}
/// Nothing here yet, said kindly, with the mark keeping it company.
fn empty<'a>(
    title_text: &'a str,
    hint: &'a str,
    extra: Option<Element<'a, Message>>,
    c: Colors,
) -> Element<'a, Message> {
    let mut body = column![
        iced::widget::image(mark())
            .width(44)
            .height(44)
            .opacity(if c.dark { 0.5_f32 } else { 0.7_f32 }),
        heading(title_text, 18),
        note(hint, c).align_x(Center),
    ]
    .spacing(10)
    .align_x(Center);
    if let Some(extra) = extra {
        body = body.push(Space::new().height(4)).push(extra);
    }
    container(body)
        .padding([32, 18])
        .width(Fill)
        .center_x(Fill)
        .style(move |_| c.card_style())
        .into()
}

impl App {
    pub fn theme(&self) -> iced::Theme {
        Colors::new(self.shell.catalog.dark).theme()
    }
    pub fn scale_factor(&self) -> f32 {
        self.settings.text_size / 14.0
    }
    pub(super) fn selected_root(&self) -> Option<&std::path::Path> {
        self.shell.catalog.selected.as_deref()
    }
    /// Whether a record belongs on the current sessions view: the selected
    /// project's, the projectless ones under Other sessions, or every one
    /// of them on the home view when nothing is selected.
    pub(super) fn has_project(&self, project: Option<&ProjectRef>) -> bool {
        match self.selected_root() {
            Some(root) => project.is_some_and(|p| p.root == root),
            None if self.shell.catalog.unassigned => project.is_none(),
            None => true,
        }
    }
    /// The home view: no project chosen, everything shown.
    pub(super) fn all_projects(&self) -> bool {
        self.selected_root().is_none() && !self.shell.catalog.unassigned
    }
    fn narrow(&self) -> bool {
        self.shell.width / self.scale_factor() < 900.0
    }
    fn in_project(&self) -> bool {
        !matches!(
            self.screen,
            Screen::Questions | Screen::Runtimes | Screen::Settings | Screen::Desktop
        )
    }
    /// The words for what an agent is doing, live or finished.
    fn activity_label(&self, agent: &AgentRecord) -> String {
        let id = agent.id.to_string();
        if self.needs_input(&id) {
            "needs input".to_owned()
        } else if self.delivery_paused(agent) {
            "delivery paused".to_owned()
        } else if agent.status.is_live() {
            match self.activity.get(&id) {
                None | Some(Activity::Unknown) => "running, no signal yet".to_owned(),
                Some(activity) => activity.label().to_owned(),
            }
        } else {
            agent.status.to_string()
        }
    }
    /// The colour that goes with [`Self::activity_label`].
    fn activity_color(&self, agent: &AgentRecord, c: Colors) -> iced::Color {
        if self.needs_input(&agent.id.to_string()) || self.delivery_paused(agent) {
            c.amber
        } else if agent.status.is_live() {
            // Green is a report, not a heartbeat: a process we only know is
            // alive has no signal to show green for.
            match self.activity.get(&agent.id.to_string()) {
                None | Some(Activity::Unknown | Activity::Starting) => c.faint,
                Some(_) => c.green,
            }
        } else {
            c.faint
        }
    }

    /// Whether any live session of this tool has reported activity: the
    /// only proof that its connection works, whatever its config says.
    fn tool_reports(&self, runtime: &str) -> bool {
        self.agents.iter().any(|a| {
            a.spec.runtime == runtime
                && a.status.is_live()
                && !matches!(
                    self.activity.get(&a.id.to_string()),
                    None | Some(Activity::Unknown | Activity::Starting)
                )
        })
    }

    pub fn view(&self) -> Element<'_, Message> {
        let c = Colors::new(self.shell.catalog.dark);
        let in_project = self.in_project();
        let narrow = self.narrow();
        let title_text = if in_project {
            self.shell
                .catalog
                .selected()
                .map(|e| e.project.name())
                .unwrap_or_else(|| {
                    if self.shell.catalog.unassigned {
                        "Other sessions"
                    } else {
                        "All projects"
                    }
                    .into()
                })
        } else {
            match self.screen {
                Screen::Questions => "Inbox",
                Screen::Runtimes => "Tools",
                _ => "Settings",
            }
            .into()
        };
        let mut heading_row = row![].spacing(10).align_y(Center);
        if in_project && let Some(entry) = self.shell.catalog.selected() {
            heading_row = heading_row.push(monogram(
                &entry.project.name(),
                &entry.project.id().to_string(),
                30.0,
                c,
            ));
        }
        heading_row = heading_row.push(title(title_text, 26));
        if in_project
            && let Some(entry) = self.shell.catalog.selected()
            && entry.pinned
        {
            heading_row = heading_row.push(pill("Pinned", c.accent_soft, c.accent_ink, c));
        }
        let mut header_left = column![heading_row].spacing(6).width(Fill);
        if in_project && let Some(entry) = self.shell.catalog.selected() {
            header_left = header_left.push(mono(entry.project.root.display().to_string(), c));
        } else if in_project && self.shell.catalog.unassigned {
            header_left = header_left.push(note("Sessions without a known project", c));
        } else if !in_project {
            header_left = header_left.push(note(
                match self.screen {
                    Screen::Questions => "Questions and messages waiting for you",
                    Screen::Runtimes => "Connect and configure your agent tools",
                    _ => "Appearance, terminal, installation and diagnostics",
                },
                c,
            ));
        }
        let mut header = row![header_left].spacing(16).align_y(Center);
        if self.screen == Screen::Agents
            && !narrow
            && let Some(launch) = self.launch_button()
        {
            header = header.push(launch);
        }
        let mut content = column![header].spacing(18).width(Fill);
        if let Err(error) = &self.connected {
            content = content.push(attention(
                column![
                    row![
                        dot(c.amber, 8.0, c),
                        heading("Connection unavailable", 15).color(c.amber)
                    ]
                    .spacing(8)
                    .align_y(Center),
                    note(
                        format!("{error}\nShowing the last update. Reconnecting…"),
                        c
                    )
                ]
                .spacing(7),
                c.amber,
                c,
            ));
        }
        if let Some(error) = &self.shell.error {
            content = content.push(attention(
                column![
                    text(error.clone()).size(14).color(c.amber),
                    action(
                        "dismiss-error",
                        "Dismiss",
                        Some(Message::DismissError),
                        false
                    )
                ]
                .spacing(10),
                c.amber,
                c,
            ));
        }
        if !self.status.is_empty() {
            content = content.push(
                row![
                    dot(c.accent, 6.0, c),
                    text(self.status.clone()).size(13).color(c.accent_ink)
                ]
                .spacing(8)
                .align_y(Center),
            );
        }
        if in_project && !self.all_projects() {
            let mut tabs = row![].spacing(14);
            for (screen, label, glyph) in [
                (Screen::Agents, "Sessions", Icon::Sessions),
                (Screen::Journal, "Activity", Icon::Activity),
            ] {
                let selected = self.screen == screen
                    || (screen == Screen::Agents && self.screen == Screen::Terminal);
                tabs = tabs.push(tab(
                    format!("project-tab-{screen:?}"),
                    label,
                    Some(icon(
                        glyph,
                        if selected { c.accent_ink } else { c.muted },
                        14.0,
                    )),
                    Some(Message::Navigate(screen)),
                    selected,
                ));
            }
            let more_selected = self.shell.more
                || matches!(
                    self.screen,
                    Screen::Channels | Screen::Leases | Screen::Console
                );
            tabs = tabs.push(tab(
                "project-more",
                "More",
                Some(icon(
                    Icon::More,
                    if more_selected { c.accent_ink } else { c.muted },
                    14.0,
                )),
                Some(Message::More),
                more_selected,
            ));
            let tabs: Element<'_, Message> = if narrow {
                scrollable(tabs)
                    .id("project-tabs")
                    .direction(iced::widget::scrollable::Direction::Horizontal(
                        iced::widget::scrollable::Scrollbar::default(),
                    ))
                    .into()
            } else {
                tabs.into()
            };
            content = content.push(column![tabs, rule(c)].spacing(0));
        }
        if in_project && self.shell.more {
            let queued = self.queued_channel_messages();
            let mut more = row![
                row![
                    icon(Icon::Channels, c.muted, 14.0),
                    action(
                        "project-tab-Channels",
                        if queued > 0 {
                            format!("Channels ({queued} waiting)")
                        } else {
                            "Channels".to_owned()
                        },
                        Some(Message::Navigate(Screen::Channels)),
                        self.screen == Screen::Channels
                    )
                ]
                .spacing(6)
                .align_y(Center),
                action(
                    "project-tab-Leases",
                    "Files in use",
                    Some(Message::Navigate(Screen::Leases)),
                    self.screen == Screen::Leases
                ),
                action(
                    "project-tab-Console",
                    "Command line",
                    Some(Message::Navigate(Screen::Console)),
                    self.screen == Screen::Console
                ),
            ]
            .spacing(6);
            if let Some(entry) = self.shell.catalog.selected() {
                more = more
                    .push(action(
                        "pin-selected",
                        if entry.pinned {
                            "Unpin project"
                        } else {
                            "Pin project"
                        },
                        Some(Message::Unpin),
                        entry.pinned,
                    ))
                    .push(action(
                        "forget-project",
                        "Forget project",
                        Some(Message::ForgetProject),
                        false,
                    ));
            }
            content = content.push(card(
                column![eyebrow("Advanced", c), more.wrap()].spacing(10),
                c,
            ));
        }
        if self.shell.adding {
            content = content.push(card(
                column![
                    heading("Add an existing project", 18),
                    note(
                        "Choose a folder. Nothing is launched or written into it.",
                        c
                    ),
                    input(
                        "project-path",
                        "Project folder",
                        &self.shell.add_path,
                        Message::AddPath
                    ),
                    row![
                        primary(
                            "pin-folder",
                            "Add project",
                            (!self.shell.add_path.trim().is_empty())
                                .then_some(Message::ResolveFolder),
                        ),
                        action("browse-folder", "Browse…", Some(Message::PickFolder), false),
                        action("cancel-add", "Cancel", Some(Message::ShowAdd), false)
                    ]
                    .spacing(6)
                ]
                .spacing(12),
                c,
            ));
        }
        let body = match self.screen {
            Screen::Agents => self.sessions(c),
            Screen::Questions => self.questions(c),
            Screen::Runtimes => self.connections(c),
            Screen::Terminal => self.terminal_view(c),
            Screen::Journal => self.journal_view(c),
            Screen::Channels => self.channels_view(c),
            Screen::Leases => self.coordination(c),
            Screen::Console => self.console_view(c),
            Screen::Settings => self.settings_view(c),
            Screen::Desktop => self.installation_view(c),
        };
        content = content.push(body);
        let workspace = row![
            self.sidebar(c),
            container(Space::new().width(1).height(Fill)).style(move |_| c.rule()),
            container(
                scrollable(content)
                    .spacing(12)
                    .width(Fill)
                    .height(Fill)
                    .id("workspace-scroll")
            )
            .padding(if narrow { [18, 18] } else { [24, 30] })
            .width(Fill)
            .style(move |_| c.surface(c.ground, false))
        ]
        .height(Fill);
        column![workspace, self.footer(c)].into()
    }

    /// One quiet line across the bottom: the daemon connection and the
    /// version. Said here once, so the rail and the pages need not repeat it.
    fn footer(&self, c: Colors) -> Element<'_, Message> {
        let connected = self.connected.is_ok();
        container(
            row![
                dot(if connected { c.green } else { c.amber }, 7.0, c),
                small(
                    if connected {
                        "Connected to the local daemon"
                    } else {
                        "Reconnecting to the local daemon…"
                    },
                    c
                )
                .width(Fill),
                match self.desktop.update_available() {
                    Some(version) => action(
                        "open-available-update",
                        format!("Update {version} available"),
                        Some(Message::Navigate(Screen::Desktop)),
                        false,
                    ),
                    None => small(format!("agentdocker {}", env!("CARGO_PKG_VERSION")), c).into(),
                }
            ]
            .spacing(8)
            .align_y(Center),
        )
        .padding([6, 14])
        .width(Fill)
        .style(move |_| container::Style {
            border: iced::Border {
                color: c.line,
                width: 1.0,
                radius: 0.0.into(),
            },
            ..c.surface(c.sidebar, false)
        })
        .into()
    }

    fn nav_item<'a>(
        &self,
        id: &'a str,
        label: &'a str,
        glyph: Icon,
        badge: Option<String>,
        message: Message,
        selected: bool,
    ) -> Element<'a, Message> {
        let c = Colors::new(self.shell.catalog.dark);
        let mut content = row![
            container(Space::new().width(3).height(16)).style(move |_| {
                c.dot(if selected {
                    c.accent
                } else {
                    iced::Color::TRANSPARENT
                })
            }),
            icon(glyph, if selected { c.accent } else { c.muted }, 16.0),
            text(label)
                .size(14)
                .font(weight(if selected {
                    iced::font::Weight::Semibold
                } else {
                    iced::font::Weight::Medium
                }))
                .width(Fill)
        ]
        .spacing(10)
        .align_y(Center);
        let spoken = match &badge {
            Some(badge) => format!("{label} {badge}"),
            None => label.to_owned(),
        };
        if let Some(badge) = badge {
            content = content.push(pill(badge, alpha(c.amber, 0.2), c.amber, c));
        }
        custom(
            id,
            spoken,
            content,
            Some(message),
            selected,
            Kind::Quiet,
            [8, 10],
        )
    }

    fn sidebar(&self, c: Colors) -> Element<'_, Message> {
        let project_page = self.in_project();
        let mut nav = column![brand(c), Space::new().height(22)].spacing(4);
        let unviewed = self.shell.unviewed_done.len();
        nav = nav.push(self.nav_item(
            "projects",
            "Projects",
            Icon::Projects,
            (unviewed > 0).then(|| unviewed.to_string()),
            Message::AllProjects,
            project_page,
        ));
        nav = nav.push(self.nav_item(
            "inbox",
            "Inbox",
            Icon::Inbox,
            {
                let waiting = self.questions.len() + self.direct_messages().len();
                (waiting > 0).then(|| waiting.to_string())
            },
            Message::Navigate(Screen::Questions),
            self.screen == Screen::Questions,
        ));
        nav = nav.push(self.nav_item(
            "connections",
            "Tools",
            Icon::Connections,
            None,
            Message::Navigate(Screen::Runtimes),
            self.screen == Screen::Runtimes,
        ));
        nav = nav
            .push(Space::new().height(18))
            .push(container(eyebrow("Projects", c)).padding([0, 12]))
            .push(Space::new().height(2));
        let mut projects = column![].spacing(2).width(Fill);
        for entry in &self.shell.catalog.projects {
            let path = entry.project.root.clone();
            let selected = project_page && self.selected_root() == Some(path.as_path());
            let name = entry.project.name();
            let live = self
                .agents
                .iter()
                .filter(|a| {
                    a.status.is_live()
                        && a.spec.runtime != agentdocker_core::HUMAN_RUNTIME
                        && a.project.as_ref().is_some_and(|p| p.root == path)
                })
                .count();
            let done = self.shell.unviewed_in(&self.agents, &path);
            let mut hint_text = if live > 0 {
                format!("{live} live agent{}", if live == 1 { "" } else { "s" })
            } else if entry.pinned {
                "Pinned · no live agents".to_owned()
            } else {
                "Discovered · no live agents".to_owned()
            };
            if done > 0 {
                hint_text.push_str(&format!(" · {done} finished, not yet viewed"));
            }
            let seed = entry.project.id().to_string();
            let mut content = row![
                hint(monogram(&name, &seed, 20.0, c), hint_text, c),
                text(name.clone()).size(14).width(Fill)
            ]
            .spacing(6)
            .align_y(Center);
            if done > 0 {
                content = content.push(pill(done.to_string(), c.accent_soft, c.accent_ink, c));
            }
            if live > 0 {
                content = content.push(
                    row![dot(c.green, 6.0, c), small(live.to_string(), c)]
                        .spacing(5)
                        .align_y(Center),
                );
            }
            projects = projects.push(custom(
                format!("project-{}", path.display()),
                format!("{}{}", if entry.pinned { "• " } else { "" }, name),
                content,
                Some(Message::SelectProject(path.clone())),
                selected,
                Kind::Quiet,
                [8, 12],
            ));
        }
        if self
            .agents
            .iter()
            .any(|a| a.project.is_none() && a.spec.runtime != agentdocker_core::HUMAN_RUNTIME)
            || self.discovered.iter().any(|p| p.project.is_none())
        {
            projects = projects.push(block_button(
                "unassigned",
                "Other sessions",
                Some(Message::Unassigned),
                self.shell.catalog.unassigned && project_page,
            ));
        }
        nav = nav
            .push(
                scrollable(projects)
                    .id("sidebar-projects")
                    .spacing(6)
                    .height(Fill),
            )
            .push(Space::new().height(6))
            .push(self.nav_item(
                "add-project",
                "Add project…",
                Icon::Add,
                None,
                Message::ShowAdd,
                self.shell.adding,
            ))
            .push(self.nav_item(
                "settings",
                "Settings",
                Icon::Settings,
                None,
                Message::Navigate(Screen::Settings),
                matches!(self.screen, Screen::Settings | Screen::Desktop),
            ))
            .push(Space::new().height(4));
        container(nav.height(Fill))
            .padding([22, 12])
            .width(if self.narrow() { 204 } else { 236 })
            .height(Fill)
            .style(move |_| c.surface(c.sidebar, false))
            .into()
    }

    /// Channel messages still queued for this person, in the projects on view.
    fn queued_channel_messages(&self) -> usize {
        let rooms: BTreeSet<_> = self
            .channels
            .iter()
            .filter(|ch| {
                self.selected_root().is_none()
                    || self
                        .shell
                        .catalog
                        .selected()
                        .is_some_and(|e| e.project.id() == ch.project)
            })
            .map(|ch| ch.id.clone())
            .collect();
        self.inbox
            .iter()
            .filter(|m| match &m.to {
                agentdocker_core::Destination::Channel(ch) => rooms.contains(ch),
                _ => false,
            })
            .count()
    }

    /// Whether an agent id belongs to the projects on view.
    fn agent_on_view(&self, id: &str) -> bool {
        self.agents
            .iter()
            .find(|a| a.id.as_str() == id)
            .is_some_and(|a| self.has_project(a.project.as_ref()))
    }

    /// Who is waiting on the person, in one strip with one action each:
    /// unanswered questions, paused delivery and sessions that finished unseen.
    /// Discovery stays in the sessions list; optional setup lives in Tools.
    /// Nothing here is new information; it is the same facts the deeper
    /// screens hold, brought to the first screen so nobody has to know
    /// where to look. Empty when nothing is waiting.
    fn needs_you(&self, c: Colors) -> Option<Element<'_, Message>> {
        const SHOWN: usize = 3;
        let now = Utc::now();
        let mut items: Vec<(Element<'_, Message>, String, Element<'_, Message>)> = Vec::new();
        for question in self
            .questions
            .iter()
            .filter(|q| !q.expired(now) && self.agent_on_view(&q.from))
        {
            items.push((
                dot(c.amber, 8.0, c),
                format!(
                    "{} asks: {}",
                    self.name_of(&question.from),
                    compact_question(&question.text)
                ),
                action(
                    format!("needs-you-answer-{}", question.id),
                    "Answer",
                    Some(Message::OpenQuestion(question.id.clone())),
                    false,
                ),
            ));
        }
        for agent in self
            .agents
            .iter()
            .filter(|a| self.delivery_paused(a) && self.has_project(a.project.as_ref()))
        {
            items.push((
                dot(c.amber, 8.0, c),
                format!("{}: message delivery needs review", agent.spec.name),
                action(
                    format!("needs-you-review-{}", agent.id),
                    "Review",
                    Some(Message::OpenSession(agent.id.to_string())),
                    false,
                ),
            ));
        }
        // Nobody waiting: on a fresh install the strip turns into the two
        // things that get a person started, then disappears for good.
        // Finished sessions are not in it; they are not asking for anything
        // (the Done pill on their row is enough).
        let guidance = items.is_empty();
        if guidance {
            for process in self.available_processes() {
                items.push((
                    dot(c.cyan, 8.0, c),
                    format!("{} is running here, not connected", process.default_name()),
                    action(
                        format!("needs-you-connect-{}", process.pid),
                        "Connect",
                        self.connected
                            .is_ok()
                            .then_some(Message::Adopt(process.pid)),
                        false,
                    ),
                ));
            }
            if self.all_projects() {
                for runtime in self.runtimes.iter().filter(|r| {
                    r.installed()
                        && !self.tool_reports(&r.name)
                        && (r.mcp == agentdocker_core::runtime::Wiring::Missing
                            || r.hooks == agentdocker_core::runtime::Wiring::Missing)
                }) {
                    items.push((
                        dot(c.faint, 8.0, c),
                        format!("{} is installed but not connected", runtime.label),
                        action(
                            format!("needs-you-tool-{}", runtime.name),
                            "Set up",
                            (!self.setup_busy).then_some(Message::Setup(vec![
                                runtime.name.clone(),
                                "--preview".into(),
                            ])),
                            false,
                        ),
                    ));
                }
            }
        }
        if items.is_empty() {
            return None;
        }
        let total = items.len();
        let title_text = if guidance {
            "To get started".to_owned()
        } else {
            format!("Needs you ({total})")
        };
        let mut list = column![row![eyebrow(title_text, c).width(Fill),]].spacing(8);
        let shown = if self.shell.needs_you_expanded {
            total
        } else {
            SHOWN
        };
        for (mark, words, act) in items.into_iter().take(shown) {
            list = list.push(
                row![mark, text(words).size(14).width(Fill), act]
                    .spacing(12)
                    .align_y(Center),
            );
        }
        if total > SHOWN {
            list = list.push(action(
                "needs-you-more",
                if self.shell.needs_you_expanded {
                    "Show fewer".to_owned()
                } else {
                    format!("Show {} more", total - SHOWN)
                },
                Some(Message::ToggleNeedsYou),
                false,
            ));
        }
        Some(attention(list, if guidance { c.cyan } else { c.amber }, c))
    }

    /// The project's one primary action, when there is a project to act in.
    fn launch_button(&self) -> Option<Element<'_, Message>> {
        self.shell.catalog.selected()?;
        Some(primary(
            "launch-agent",
            "Launch agent…",
            (self.connected.is_ok() && self.shell.project_available != Some(false))
                .then_some(Message::ShowLaunch),
        ))
    }

    fn sessions(&self, c: Colors) -> Element<'_, Message> {
        let selected = self.shell.selected.as_ref().and_then(|id| {
            self.agents
                .iter()
                .find(|a| a.id.as_str() == self.canonical_agent(id))
        });
        let mut panel_col = column![]
            .spacing(if self.settings.roomy { 20 } else { 14 })
            .width(Fill);
        use super::sessions::Filter;
        let current = self.session_records(Filter::Current);
        let attention_rows = self.session_records(Filter::NeedsInput);
        let history = self.session_records(Filter::History);
        let available = self.available_processes();
        let filter = self.shell.session_filter;
        let filters = segmented(
            vec![
                segment(
                    "sessions-current",
                    format!("Current ({})", current.len() + available.len()),
                    Some(Message::SessionFilter(Filter::Current)),
                    filter == Filter::Current,
                ),
                segment(
                    "sessions-attention",
                    format!("Needs input ({})", attention_rows.len()),
                    Some(Message::SessionFilter(Filter::NeedsInput)),
                    filter == Filter::NeedsInput,
                ),
                segment(
                    "sessions-history",
                    format!("History ({})", history.len()),
                    Some(Message::SessionFilter(Filter::History)),
                    filter == Filter::History,
                ),
            ],
            c,
        );
        let search = input(
            "session-search",
            "Find a session…",
            &self.shell.search,
            Message::Search,
        );
        if self.narrow() {
            let mut top = row![search].spacing(10).align_y(Center);
            if let Some(launch) = self.launch_button() {
                top = top.push(launch);
            }
            panel_col = panel_col.push(column![top, filters].spacing(10));
        } else if selected.is_some() {
            // The inspector takes 340 px from this column. Keep the filter
            // labels and counts together instead of squeezing them beside search.
            panel_col = panel_col.push(column![filters, search].spacing(10));
        } else {
            panel_col = panel_col.push(
                row![container(filters).width(Fill), container(search).width(260)]
                    .spacing(10)
                    .align_y(Center),
            );
        }
        if let Some(strip) = self.needs_you(c) {
            panel_col = panel_col.push(strip);
        }
        if self.shell.project_available == Some(false) {
            panel_col = panel_col.push(attention(
                column![
                    heading("Project folder unavailable", 16),
                    note("Restore the folder or choose another project.", c),
                    action(
                        "retry-project-folder",
                        "Check folder again",
                        Some(Message::RetryProject),
                        false
                    )
                ]
                .spacing(10),
                c.amber,
                c,
            ));
        }
        if self.shell.launch {
            panel_col = panel_col.push(self.launch_view(c));
        }
        let records = match filter {
            Filter::Current => current,
            Filter::NeedsInput => attention_rows,
            Filter::History => history,
        };
        let count = records.len()
            + if filter == Filter::Current {
                available.len()
            } else {
                0
            };
        let mut rows = column![].spacing(if self.settings.roomy { 6 } else { 2 });
        let mut previous_project = None;
        for (index, agent) in records.iter().enumerate() {
            let project_root = agent.project.as_ref().map(|p| &p.root);
            if self.all_projects() && (index == 0 || previous_project != project_root) {
                rows = rows.push(
                    container(eyebrow(
                        agent
                            .project
                            .as_ref()
                            .map(|p| p.name())
                            .unwrap_or_else(|| "Other sessions".into()),
                        c,
                    ))
                    .padding([12, 12]),
                );
                previous_project = project_root;
            }
            let id = agent.id.to_string();
            let activity = self.activity_label(agent);
            let branch = agent.vcs.as_ref().and_then(|v| v.branch.as_deref());
            let meta = format!(
                "{}{}",
                branch.map(|b| format!("{b} · ")).unwrap_or_default(),
                activity
            );
            let spoken = format!("{}\n{} · {}", agent.spec.name, agent.spec.runtime, meta);
            let mut lines = column![
                text(agent.spec.name.clone())
                    .size(14)
                    .font(weight(iced::font::Weight::Medium)),
                small(meta, c)
            ]
            .spacing(2)
            .width(Fill);
            if let Some(left) = self.soonest_answer_window(&id) {
                lines = lines.push(
                    container(meter(left, if left < 0.2 { c.red } else { c.amber }, c))
                        .width(160)
                        .padding([3, 0]),
                );
            }
            let mut content = row![dot(self.activity_color(agent, c), 8.0, c)]
                .spacing(12)
                .align_y(Center);
            content = content.push(lines);
            if self.shell.unviewed_done.contains(&id) {
                // Finished since you last looked. The observed state stays
                // in the meta line; this says only that it is new to you.
                content = content.push(pill("Done", c.accent_soft, c.accent_ink, c));
            }
            content = content.push(pill(agent.spec.runtime.clone(), c.raised, c.muted, c));
            rows = rows.push(custom(
                format!("session-{id}"),
                spoken,
                content,
                Some(if self.all_projects() {
                    Message::OpenSession(id.clone())
                } else {
                    Message::SelectSession(id.clone())
                }),
                self.shell.selected.as_deref() == Some(id.as_str()),
                Kind::Quiet,
                [if self.settings.roomy { 13 } else { 10 }, 12],
            ));
        }
        if !records.is_empty() {
            panel_col = panel_col.push(panel(rows, c));
        }
        if filter == Filter::Current && !available.is_empty() {
            let mut discovered = column![
                eyebrow("Running here, not connected", c),
                note(
                    "Started outside AgentDocker. Connect one to see what it is doing and message it.",
                    c
                )
            ]
            .spacing(6);
            for process in available {
                discovered = discovered.push(
                    row![
                        dot(c.cyan, 8.0, c),
                        column![
                            text(process.default_name()).size(14),
                            small(process.runtime.clone(), c)
                        ]
                        .spacing(2)
                        .width(Fill),
                        action(
                            format!("adopt-{}", process.pid),
                            "Connect",
                            self.connected
                                .is_ok()
                                .then_some(Message::Adopt(process.pid)),
                            false
                        ),
                    ]
                    .spacing(12)
                    .align_y(Center),
                );
            }
            panel_col = panel_col.push(card(discovered.spacing(10), c));
        }
        if count == 0 {
            let (title_text, hint) = if !self.shell.search.is_empty() {
                ("No matching sessions", "Try another name, tool, or branch.")
            } else {
                match filter {
                    Filter::Current if self.all_projects() => (
                        "No agents running",
                        "Start Claude Code or Codex in any folder and it appears here, \
                         or choose a project on the left and launch one.",
                    ),
                    Filter::Current => (
                        "No agents in this project",
                        "Press Launch agent, or start Claude Code or Codex in this folder \
                         and it appears here.",
                    ),
                    Filter::NeedsInput => (
                        "Nothing needs your input",
                        "When an agent asks you something, it appears here and in Inbox.",
                    ),
                    Filter::History => (
                        "No finished sessions",
                        "Completed sessions will appear here.",
                    ),
                }
            };
            panel_col = panel_col.push(empty(title_text, hint, None, c));
        }
        if let Some(agent) = selected {
            let inspector = self.inspector(agent, c);
            if self.shell.width / self.scale_factor() >= 1120.0 {
                return row![panel_col, container(inspector).width(320)]
                    .spacing(20)
                    .into();
            }
            return inspector;
        }
        panel_col.into()
    }

    fn inspector(&self, agent: &AgentRecord, c: Colors) -> Element<'_, Message> {
        let id = agent.id.to_string();
        let stop_armed = self
            .confirm_stop
            .as_ref()
            .is_some_and(|(armed, at)| armed == &id && at.elapsed() < CONFIRM_WITHIN);
        let mut body = column![
            row![
                dot(self.activity_color(agent, c), 10.0, c),
                column![
                    heading(agent.spec.name.clone(), 18),
                    note(
                        format!("{} · {}", agent.spec.runtime, self.activity_label(agent)),
                        c
                    )
                ]
                .spacing(3)
            ]
            .spacing(10)
            .align_y(Center),
            rule(c),
        ]
        .spacing(12);
        let delivery = agent.input_delivery.as_ref();
        let paused = delivery.is_some_and(|d| d.paused_for(agent.process_started_at));
        if agent.spec.runtime != "human" {
            let queue = self.queued_inputs.get(&id).map(|count| {
                if *count == 0 {
                    "No queued messages".to_owned()
                } else {
                    format!("{count} queued")
                }
            });
            let mut status = Vec::new();
            if self.connected.is_err() {
                status.push("Last known".to_owned());
            }
            if paused {
                status.push("Delivery paused".to_owned());
            }
            if let Some(queue) = queue {
                status.push(queue);
            }
            if let Some(received_at) = delivery.and_then(|d| d.received_at) {
                status.push(format!("Last receipt {}", ago(Utc::now(), received_at)));
            }
            if !status.is_empty() {
                body = body.push(small(status.join(" · "), c));
            }
            if paused {
                body = body.push(action(
                    "review-delivery",
                    if self.shell.review_delivery {
                        "Hide review"
                    } else {
                        "Review delivery"
                    },
                    self.connected.is_ok().then_some(Message::ReviewDelivery),
                    false,
                ));
                if self.shell.review_delivery {
                    if let Some(reason) = delivery.and_then(|d| d.pause_reason.as_deref()) {
                        body = body.push(text(reason.to_owned()).size(13).color(c.amber));
                    }
                    body = body.push(note("Input is retained. Check the receipt and session log before restarting or sending it again.", c));
                    if let Some((log_agent, result)) = &self.session_log
                        && log_agent == &id
                    {
                        match result {
                            Err(error) => {
                                body = body.push(
                                    text(format!("Could not read the session log: {error}"))
                                        .size(13)
                                        .color(c.amber),
                                );
                            }
                            Ok(log) => {
                                let log = if log.is_empty() {
                                    "No retained log output."
                                } else {
                                    log.as_str()
                                };
                                body = body.push(scrollable(text(log).size(12)).height(120));
                            }
                        }
                    } else {
                        body = body.push(small("Loading session log…", c));
                    }
                }
            }
        }
        if self.needs_input(&id) {
            body = body.push(primary(
                "session-reply",
                "Reply in Inbox",
                Some(Message::Navigate(Screen::Questions)),
            ));
        }
        if agent.managed && agent.spec.tty && agent.status.is_live() {
            body = body.push(primary(
                "attach-session",
                "Open terminal",
                self.connected
                    .is_ok()
                    .then_some(Message::Attach(id.clone())),
            ));
        } else if agent.status.is_live() {
            body = body.push(note(
                "Continue in the app or terminal where this agent started.",
                c,
            ));
        }
        if agent.status.is_live() && agent.spec.runtime != "human" {
            body = body.push(action(
                "session-message",
                if self.shell.session_message {
                    "Hide message"
                } else {
                    "Message"
                },
                Some(Message::ComposeSession),
                self.shell.session_message,
            ));
            if self.shell.session_message {
                let draft_key = self.session_draft_key(&id);
                let entry = self.shell.session_drafts.get(&draft_key);
                let draft = entry.map(|entry| &entry.draft);
                let sending = draft.is_some_and(|draft| draft.sending.is_some());
                let value = draft.map_or("", |draft| draft.text.as_str());
                let target = draft_key.clone();
                body = body
                    .push(input(
                        "session-message-text",
                        "Message this agent…",
                        value,
                        move |text| Message::SessionDraft(target.clone(), text),
                    ))
                    .push(primary(
                        "send-session-message",
                        if sending {
                            "Queueing…"
                        } else {
                            "Send message"
                        },
                        (!sending && !value.trim().is_empty() && self.connected.is_ok())
                            .then_some(Message::SendSession(draft_key)),
                    ));
                if let Some(error) = draft.and_then(|draft| draft.error.as_deref()) {
                    body = body.push(text(error).size(13).color(c.amber));
                } else if entry.is_some_and(|entry| entry.queued.is_some()) {
                    let received =
                        entry
                            .and_then(|entry| entry.queued.as_ref())
                            .is_some_and(|id| {
                                delivery
                                    .and_then(|d| d.received.as_ref())
                                    .is_some_and(|input| input.messages.contains(id))
                            });
                    body = body.push(small(
                        if received {
                            "Received by agent"
                        } else {
                            "Message saved to queue"
                        },
                        c,
                    ));
                }
            }
        }
        body = body.push(action(
            "session-details",
            if self.shell.session_details {
                "Hide details"
            } else {
                "Details"
            },
            Some(Message::SessionDetails),
            self.shell.session_details,
        ));
        if self.shell.session_details {
            let mut details = column![kv("Session", agent.id.to_string(), c)].spacing(6);
            details = details.push(kv(
                "Folder",
                agent
                    .spec
                    .workdir
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "Working folder unknown".into()),
                c,
            ));
            details = details.push(kv("Last seen", ago(Utc::now(), agent.last_seen), c));
            if let Some(pid) = agent.pid {
                details = details.push(kv("Process", pid.to_string(), c));
            }
            if let Some(vcs) = &agent.vcs {
                details = details.push(kv("Checkout", vcs.describe(), c));
            }
            if let Some(session) = &agent.session {
                details = details.push(kv("Terminal", format!("{session:?}"), c));
            }
            body = body.push(details);
        }
        if agent.status.is_live() {
            body = body.push(if stop_armed {
                danger(
                    "stop-session",
                    "Confirm stop",
                    self.connected.is_ok().then_some(Message::Stop(id)),
                )
            } else {
                action(
                    "stop-session",
                    "Stop session…",
                    self.connected.is_ok().then_some(Message::Stop(id)),
                    false,
                )
            });
        }
        if stop_armed {
            body = body.push(note(
                "Confirm within five seconds to send the stop signal.",
                c,
            ));
        }
        body = body.push(rule(c)).push(action(
            "close-session",
            "Back to sessions",
            Some(Message::CloseSession),
            false,
        ));
        card(body, c)
    }

    fn launch_view(&self, c: Colors) -> Element<'_, Message> {
        let mut tools = column![
            heading("Launch in this project", 18),
            note("Choose a tool to start in this folder.", c)
        ]
        .spacing(10);
        let mut choices = row![].spacing(6);
        for runtime in self.runtimes.iter().filter(|r| r.cli.is_some()) {
            choices = choices.push(action(
                format!("launch-tool-{}", runtime.name),
                runtime.label.clone(),
                (!self.shell.launching).then_some(Message::LaunchRuntime(runtime.name.clone())),
                self.shell.launch_runtime.as_deref() == Some(runtime.name.as_str()),
            ));
        }
        tools = tools
            .push(choices.wrap())
            .push(input(
                "launch-name",
                "Session name (optional)",
                &self.shell.launch_name,
                Message::LaunchName,
            ))
            .push(input(
                "launch-arguments",
                "Command arguments (optional)",
                &self.shell.launch_arguments,
                Message::LaunchArguments,
            ));
        if matches!(
            self.shell.launch_runtime.as_deref(),
            Some("claude-code" | "codex")
        ) {
            // Explicit and per launch: nothing on disk changes, and an
            // existing session is never taken over.
            tools = tools.push(
                column![
                    iced::widget::checkbox(self.shell.launch_channel)
                        .label("Receive messages while idle (experimental)")
                        .on_toggle(Message::LaunchChannel)
                        .size(16)
                        .text_size(13),
                    small(
                        if self.shell.launch_runtime.as_deref() == Some("codex") {
                            "Opens a Codex conversation here. Messages wait until the current turn finishes."
                        } else { "Replies reach this session between turns. Requires Claude consent \
                         in the terminal. Applies to this new session only." },
                        c
                    )
                ]
                .spacing(4),
            );
        }
        if let Some(runtime) = self
            .runtimes
            .iter()
            .find(|r| Some(&r.name) == self.shell.launch_runtime.as_ref())
        {
            tools = tools.push(mono(
                format!(
                    "{} {}",
                    runtime
                        .cli
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                    self.shell.launch_arguments
                ),
                c,
            ));
        }
        tools = tools.push(
            row![
                primary(
                    "confirm-launch",
                    if self.shell.launching {
                        "Launching…"
                    } else {
                        "Launch agent"
                    },
                    (!self.shell.launching
                        && self.shell.launch_runtime.is_some()
                        && self.connected.is_ok())
                    .then_some(Message::Launch),
                ),
                action(
                    "cancel-launch",
                    "Close",
                    (!self.shell.launching).then_some(Message::ShowLaunch),
                    false
                )
            ]
            .spacing(6),
        );
        card(tools, c)
    }

    /// Messages sent to the person directly, oldest first, minus the ones
    /// that are questions (those have their own cards).
    fn direct_messages(&self) -> Vec<&agentdocker_core::Envelope> {
        let (_, mut direct) = by_room(&self.inbox);
        direct.retain(|message| !self.questions.iter().any(|q| q.id == message.id));
        direct
    }

    /// Inbox as a messenger: a conversation per agent on the left, the
    /// chosen conversation as bubbles on the right, a composer under it.
    /// Questions keep their cards, at the top of the agent's conversation,
    /// because they carry controls a bubble cannot.
    fn questions(&self, c: Colors) -> Element<'_, Message> {
        let now = Utc::now();
        let direct = self.direct_messages();
        // Conversations, newest activity first.
        let mut threads: Vec<(String, chrono::DateTime<Utc>, usize)> = Vec::new();
        let mut note_thread = |who: &str, at: chrono::DateTime<Utc>, waiting: bool| {
            let who = self.canonical_agent(who).to_owned();
            match threads.iter_mut().find(|(id, _, _)| *id == who) {
                Some(entry) => {
                    entry.1 = entry.1.max(at);
                    entry.2 += usize::from(waiting);
                }
                None => threads.push((who, at, usize::from(waiting))),
            }
        };
        for message in &direct {
            note_thread(&message.from, message.sent_at, true);
        }
        for question in &self.questions {
            note_thread(&question.from, question.asked_at, !question.expired(now));
        }
        threads.sort_by_key(|thread| std::cmp::Reverse(thread.1));
        let selected = self
            .shell
            .inbox_thread
            .as_deref()
            .filter(|id| threads.iter().any(|(t, _, _)| t == id));

        if threads.is_empty() {
            return empty(
                "You're all caught up",
                "Questions and messages from your agents appear here, one conversation per agent.",
                None,
                c,
            );
        }

        // Left: who is talking to you.
        let mut list = column![].spacing(2);
        let total_waiting: usize = threads.iter().map(|t| t.2).sum();
        let mut everyone = row![
            monogram("Everyone", "everyone", 28.0, c),
            column![
                text("Everyone")
                    .size(14)
                    .font(weight(iced::font::Weight::Medium)),
                small(
                    format!(
                        "{} conversation{}",
                        threads.len(),
                        if threads.len() == 1 { "" } else { "s" }
                    ),
                    c
                )
            ]
            .spacing(1)
            .width(Fill)
        ]
        .spacing(10)
        .align_y(Center);
        if total_waiting > 0 {
            everyone = everyone.push(pill(
                total_waiting.to_string(),
                c.accent_soft,
                c.accent_ink,
                c,
            ));
        }
        list = list.push(custom(
            "thread-everyone",
            "Everyone",
            everyone,
            Some(Message::SelectThread(None)),
            selected.is_none(),
            Kind::Quiet,
            [8, 10],
        ));
        for (id, at, waiting) in &threads {
            let name = self.name_of(id);
            let preview = direct
                .iter()
                .rev()
                .find(|m| self.canonical_agent(&m.from) == id)
                .map(|m| first_line(&spoken_payload(&m.payload), 48))
                .or_else(|| {
                    self.questions
                        .iter()
                        .rev()
                        .find(|q| self.canonical_agent(&q.from) == id)
                        .map(|q| first_line(&q.text, 48))
                })
                .unwrap_or_default();
            let mut row_content = row![
                monogram(&name, id, 28.0, c),
                column![
                    row![
                        text(name.clone())
                            .size(14)
                            .font(weight(iced::font::Weight::Medium))
                            .width(Fill),
                        small(ago(now, *at), c)
                    ]
                    .spacing(6)
                    .align_y(Center),
                    small(preview, c)
                ]
                .spacing(1)
                .width(Fill)
            ]
            .spacing(10)
            .align_y(Center);
            if *waiting > 0 {
                row_content =
                    row_content.push(pill(waiting.to_string(), c.accent_soft, c.accent_ink, c));
            }
            list = list.push(custom(
                format!("thread-{id}"),
                name,
                row_content,
                Some(Message::SelectThread(Some(id.clone()))),
                selected == Some(id.as_str()),
                Kind::Quiet,
                [8, 10],
            ));
        }

        // Right: the conversation.
        let mut convo = column![].spacing(10).width(Fill);
        for question in self
            .questions
            .iter()
            .filter(|q| selected.is_none_or(|id| self.canonical_agent(&q.from) == id))
        {
            convo = convo.push(self.question_card(question, c));
        }
        let shown: Vec<_> = direct
            .iter()
            .copied()
            .filter(|m| selected.is_none_or(|id| self.canonical_agent(&m.from) == id))
            .collect();
        let shown = self.recent_window(&shown, 30);
        if shown.is_empty() && self.questions.is_empty() {
            convo = convo.push(note("No messages yet.", c));
        }
        let ids: Vec<_> = shown.iter().map(|m| m.id.clone()).collect();
        if ids.len() > 1 {
            let enabled =
                self.connected.is_ok() && ids.iter().all(|id| !self.dismissing.contains(id));
            convo = convo.push(row![
                Space::new().width(Fill),
                action(
                    format!("dismiss-shown-{}", ids[0]),
                    "Clear shown",
                    enabled.then_some(Message::DismissInbox(ids.clone())),
                    false,
                )
            ]);
        }
        let mut last_from: Option<String> = None;
        for message in shown {
            let from = self.canonical_agent(&message.from).to_owned();
            let show_name = last_from.as_deref() != Some(from.as_str()) || selected.is_none();
            last_from = Some(from.clone());
            convo = convo.push(self.bubble(message, show_name, c));
        }
        // Composer: reply to this agent, or to everyone in its project.
        if let Some(id) = selected {
            let live = self
                .agents
                .iter()
                .any(|a| a.id.as_str() == id && a.status.is_live());
            let entry = self.shell.session_drafts.get(id);
            let draft = entry.map(|e| e.draft.text.clone()).unwrap_or_default();
            let sending = entry.is_some_and(|e| e.draft.sending.is_some());
            let ready = live && self.connected.is_ok() && !sending && !draft.trim().is_empty();
            let asked = self
                .questions
                .iter()
                .any(|q| self.canonical_agent(&q.from) == id && !q.expired(now));
            let owner = id.to_owned();
            let mut composer = column![
                row![
                    input_enabled(
                        format!("reply-{id}"),
                        if asked {
                            "Answer or reply…"
                        } else {
                            "Message…"
                        },
                        &draft,
                        move |t| Message::SessionDraft(owner.clone(), t),
                        live && !sending,
                    ),
                    primary(
                        format!("send-reply-{id}"),
                        if sending { "Sending…" } else { "Send" },
                        ready.then_some(Message::SendSession(id.to_owned())),
                    ),
                    action(
                        format!("send-everyone-{id}"),
                        "Send to everyone",
                        ready.then_some(Message::SendProject(id.to_owned())),
                        false,
                    )
                ]
                .spacing(8)
                .align_y(Center)
            ]
            .spacing(6);
            if !live {
                composer = composer.push(small(
                    "This agent is not running, so it cannot receive a message.",
                    c,
                ));
            } else if let Some(error) = entry.and_then(|e| e.draft.error.as_ref()) {
                composer = composer.push(text(error.clone()).size(13).color(c.amber));
            } else if entry.is_some_and(|e| e.queued.is_some()) && !sending {
                composer = composer.push(small("Delivered to the agent's queue.", c));
            } else {
                composer = composer.push(small(
                    "Send goes to this agent. Send to everyone reaches every agent in its project.",
                    c,
                ));
            }
            convo = convo.push(composer);
        } else {
            convo = convo.push(small("Choose a conversation to reply.", c));
        }

        if self.narrow() {
            column![panel(list, c), convo].spacing(14).into()
        } else {
            row![
                container(panel(list, c)).width(300),
                container(convo).width(Fill)
            ]
            .spacing(16)
            .into()
        }
    }

    /// One message as a chat bubble: sender and time on top, the text
    /// beneath, long texts folded until asked for.
    fn bubble(
        &self,
        message: &agentdocker_core::Envelope,
        show_name: bool,
        c: Colors,
    ) -> Element<'_, Message> {
        const FOLD: usize = 420;
        let id = message.id.clone();
        let payload = spoken_payload(&message.payload);
        let long = payload.chars().count() > FOLD || payload.lines().count() > 8;
        let expanded = self.shell.message_detail.as_ref() == Some(&id);
        let shown_text = if long && !expanded {
            let head: String = payload.lines().take(8).collect::<Vec<_>>().join("\n");
            let head: String = head.chars().take(FOLD).collect();
            format!("{head}…")
        } else {
            payload
        };
        let mut head = row![].spacing(8).align_y(Center);
        if show_name {
            head = head.push(
                text(self.name_of(&message.from))
                    .size(13)
                    .font(weight(iced::font::Weight::Semibold)),
            );
        }
        if message.kind != "chat" && message.kind != "message" {
            head = head.push(pill(message.kind.to_string(), c.raised, c.muted, c));
        }
        head = head.push(small(ago(Utc::now(), message.sent_at), c));
        head = head.push(Space::new().width(Fill));
        if self.inbox.iter().any(|m| m.id == id) {
            let busy = self.dismissing.contains(&id);
            head = head.push(action(
                format!("dismiss-message-{id}"),
                if busy { "…" } else { "Clear" },
                (!busy && self.connected.is_ok())
                    .then_some(Message::DismissInbox(vec![id.clone()])),
                false,
            ));
        }
        let mut body = column![head, text(shown_text).size(14)].spacing(4);
        if long {
            body = body.push(action(
                format!("message-detail-{id}"),
                if expanded { "Show less" } else { "Show more" },
                Some(Message::QuestionDetails(id.clone())),
                false,
            ));
        }
        container(
            container(body)
                .padding([10, 14])
                .max_width(760)
                .style(move |_| container::Style {
                    background: Some(c.raised.into()),
                    border: iced::Border {
                        radius: 14.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
        )
        .width(Fill)
        .id(format!("notification-message-{id}"))
        .into()
    }

    /// One question with its controls.
    fn question_card(&self, question: &Question, c: Colors) -> Element<'_, Message> {
        {
            let id = question.id.clone();
            let draft_id = id.clone();
            let busy = self.sending.contains(&id);
            let expired = question.expired(Utc::now());
            let answer = self.answers.get(&id).cloned().unwrap_or_default();
            let mut body = column![
                row![
                    dot(if expired { c.faint } else { c.amber }, 8.0, c),
                    eyebrow(
                        format!(
                            "{} · asked {}",
                            self.name_of(&question.from),
                            ago(Utc::now(), question.asked_at)
                        ),
                        c
                    )
                ]
                .spacing(8)
                .align_y(Center),
            ]
            .spacing(10);
            let presentation = question
                .presentation
                .as_ref()
                .filter(|p| p.valid_for(&question.text));
            let enabled = !busy && !expired && self.connected.is_ok();
            match presentation {
                Some(agentdocker_core::QuestionPresentation::CodexPermissions {
                    cwd,
                    reason,
                    permissions,
                }) => {
                    body = body
                        .push(heading("Allow additional access?", 18))
                        .push(mono(format!("Folder: {cwd}"), c));
                    if !reason.trim().is_empty() && reason != "Requested by Codex" {
                        body = body.push(note(reason.clone(), c));
                    }
                    for line in permissions.lines() {
                        body = body.push(mono(line, c));
                    }
                    body = body.push(self.answer_window(question, c)).push(
                        row![
                            primary(
                                format!("answer-allow-{id}"),
                                if busy {
                                    "Sending…"
                                } else {
                                    "Allow for this turn"
                                },
                                enabled.then(|| Message::AnswerChoice(id.clone(), "Allow".into()))
                            ),
                            action(
                                format!("answer-deny-{id}"),
                                "Deny",
                                enabled.then(|| Message::AnswerChoice(id.clone(), "Deny".into())),
                                false
                            ),
                        ]
                        .spacing(8)
                        .align_y(Center),
                    );
                }
                Some(agentdocker_core::QuestionPresentation::CodexFiles {
                    cwd,
                    reason,
                    changes,
                }) => {
                    let expanded = self.shell.file_review.as_ref() == Some(&id);
                    body = body
                        .push(heading("Apply these file changes?", 18))
                        .push(mono(format!("Folder: {cwd}"), c));
                    if !reason.trim().is_empty() && reason != "Requested by Codex" {
                        body = body.push(note(reason.clone(), c));
                    }
                    if !expanded {
                        for change in changes {
                            body = body.push(mono(change.label(), c));
                        }
                    }
                    body = body.push(action(
                        format!("review-files-{id}"),
                        if expanded {
                            "Hide changes"
                        } else {
                            "Review changes"
                        },
                        Some(Message::ReviewFiles(id.clone())),
                        false,
                    ));
                    if expanded {
                        let mut details = column![].spacing(12);
                        for change in changes {
                            details = details.push(mono(change.label(), c)).push(
                                text(change.diff.clone())
                                    .font(Font::MONOSPACE)
                                    .size(13)
                                    .color(c.text),
                            );
                        }
                        body = body.push(
                            container(scrollable(details).height(iced::Shrink)).max_height(320),
                        );
                    }
                    body = body.push(self.answer_window(question, c)).push(
                        row![
                            primary(
                                format!("answer-allow-{id}"),
                                if busy { "Sending…" } else { "Allow once" },
                                (enabled && expanded)
                                    .then(|| Message::AnswerChoice(id.clone(), "Allow".into()))
                            ),
                            action(
                                format!("answer-deny-{id}"),
                                "Deny",
                                enabled.then(|| Message::AnswerChoice(id.clone(), "Deny".into())),
                                false
                            ),
                        ]
                        .spacing(8)
                        .align_y(Center),
                    );
                }
                Some(agentdocker_core::QuestionPresentation::CodexCommand {
                    command,
                    cwd,
                    reason,
                }) => {
                    body = body
                        .push(heading("Run this command?", 18))
                        .push(
                            text(command.clone())
                                .size(14)
                                .font(Font::MONOSPACE)
                                .color(c.text),
                        )
                        .push(mono(format!("Folder: {cwd}"), c));
                    if !reason.trim().is_empty() {
                        body = body.push(note(reason.clone(), c));
                    }
                    body = body.push(self.answer_window(question, c)).push(
                        row![
                            primary(
                                format!("answer-allow-{id}"),
                                if busy { "Sending…" } else { "Allow once" },
                                enabled.then(|| Message::AnswerChoice(id.clone(), "Allow".into()))
                            ),
                            action(
                                format!("answer-deny-{id}"),
                                "Deny",
                                enabled.then(|| Message::AnswerChoice(id.clone(), "Deny".into())),
                                false
                            ),
                        ]
                        .spacing(8)
                        .align_y(Center),
                    );
                }
                _ => {
                    if let Some(agentdocker_core::QuestionPresentation::Choices {
                        question: prompt,
                        options,
                    }) = presentation
                    {
                        body = body
                            .push(heading(prompt.clone(), 18))
                            .push(self.answer_window(question, c));
                        for (index, option) in options.iter().enumerate() {
                            body = body.push(action(
                                format!("answer-choice-{id}-{index}"),
                                option.label.clone(),
                                enabled.then(|| {
                                    Message::AnswerChoice(id.clone(), option.label.clone())
                                }),
                                false,
                            ));
                            if !option.description.trim().is_empty() {
                                body = body.push(note(option.description.clone(), c));
                            }
                        }
                    } else {
                        body = body
                            .push(heading(question.text.clone(), 18))
                            .push(self.answer_window(question, c));
                    }
                    body = body
                        .push(input_enabled(
                            format!("answer-{id}"),
                            if presentation.is_some() {
                                "Or write an answer"
                            } else {
                                "Your answer"
                            },
                            &answer,
                            move |text| Message::Draft(draft_id.clone(), text),
                            enabled,
                        ))
                        .push(primary(
                            format!("send-answer-{id}"),
                            if busy { "Sending…" } else { "Send answer" },
                            (enabled && !answer.trim().is_empty())
                                .then_some(Message::Answer(id.clone())),
                        ));
                }
            }
            if expired {
                body = body.push(note("This question has expired.", c));
            }
            if let Some(error) = self.shell.answer_errors.get(&id) {
                body = body.push(text(error.clone()).size(13).color(c.amber));
            }
            container(if expired {
                card(body, c)
            } else {
                attention(body, c.amber, c)
            })
            .id(format!("notification-question-{id}"))
            .into()
        }
    }

    /// The least time left among this agent's unanswered questions, as a
    /// fraction of its window; `None` when nothing is waiting.
    fn soonest_answer_window(&self, agent: &str) -> Option<f32> {
        let now = Utc::now();
        self.questions
            .iter()
            .filter(|q| q.from == agent && !q.expired(now))
            .map(|q| remaining_fraction(q.asked_at, q.expires_at, now))
            .min_by(|a, b| a.total_cmp(b))
    }

    /// How long is left to answer, as a meter and a few words. An expired
    /// question says so instead.
    fn answer_window(&self, question: &Question, c: Colors) -> Element<'_, Message> {
        let now = Utc::now();
        let left_secs = (question.expires_at - now).num_seconds();
        if left_secs <= 0 {
            return small("No longer waiting for an answer", c).into();
        }
        let left = remaining_fraction(question.asked_at, question.expires_at, now);
        row![
            meter(left, if left < 0.2 { c.red } else { c.amber }, c),
            small(format!("{} left to answer", super::span(left_secs)), c)
        ]
        .spacing(10)
        .align_y(Center)
        .into()
    }

    /// The last `keep` messages, plus the one a notification pointed at when
    /// it is older than that, so the anchor it scrolls to exists.
    fn recent_window<'a>(
        &self,
        messages: &[&'a agentdocker_core::Envelope],
        keep: usize,
    ) -> Vec<&'a agentdocker_core::Envelope> {
        let start = messages.len().saturating_sub(keep);
        let mut window: Vec<_> = messages[start..].to_vec();
        if let Some(wanted) = &self.shell.notification_message
            && let Some(older) = messages[..start].iter().find(|m| m.id == *wanted)
        {
            window.insert(0, older);
        }
        window
    }

    /// Messages as a transcript: when, who, what — one line each, the way
    /// a chat client reads, rather than a card per message.
    fn transcript<'a>(
        &'a self,
        messages: impl Iterator<Item = &'a agentdocker_core::Envelope>,
        c: Colors,
    ) -> Element<'a, Message> {
        let messages: Vec<_> = messages.collect();
        let dismissible: Vec<_> = messages
            .iter()
            .filter(|message| {
                self.inbox.iter().any(|item| item.id == message.id)
                    && !self
                        .questions
                        .iter()
                        .any(|question| question.id == message.id)
            })
            .map(|message| message.id.clone())
            .collect();
        let mut lines = column![].spacing(0);
        if dismissible.len() > 1 {
            let enabled = self.connected.is_ok()
                && dismissible.iter().all(|id| !self.dismissing.contains(id));
            lines = lines.push(
                row![
                    Space::new().width(Fill),
                    action(
                        format!("dismiss-shown-{}", dismissible[0]),
                        "Dismiss shown",
                        enabled.then_some(Message::DismissInbox(dismissible.clone())),
                        false,
                    )
                ]
                .padding([4, 10]),
            );
        }
        let mut first = true;
        for message in messages {
            if !first {
                lines = lines.push(container(rule(c)).padding([0, 10]));
            }
            first = false;
            let payload = spoken_payload(&message.payload);
            let earlier_question = message.kind == "question"
                && matches!(message.to, agentdocker_core::Destination::Agent(_))
                && dismissible.contains(&message.id);
            let expanded = self.shell.message_detail.as_ref() == Some(&message.id);
            let mut who = row![
                text(self.name_of(&message.from))
                    .size(13)
                    .font(weight(iced::font::Weight::Semibold))
            ]
            .spacing(8)
            .align_y(Center);
            if message.kind.to_string().as_str() != "chat" {
                who = who.push(pill(
                    if earlier_question {
                        "Earlier question".into()
                    } else {
                        message.kind.to_string()
                    },
                    c.raised,
                    c.muted,
                    c,
                ));
            }
            if self.inbox.iter().any(|item| item.id == message.id)
                && !self
                    .questions
                    .iter()
                    .any(|question| question.id == message.id)
            {
                let busy = self.dismissing.contains(&message.id);
                who = who.push(Space::new().width(Fill)).push(action(
                    format!("dismiss-message-{}", message.id),
                    if busy { "Dismissing…" } else { "Dismiss" },
                    (!busy && self.connected.is_ok())
                        .then_some(Message::DismissInbox(vec![message.id.clone()])),
                    false,
                ));
            }
            let mut content = column![who].spacing(3).width(Fill);
            if earlier_question {
                if expanded {
                    content = content.push(text(payload).size(13));
                } else {
                    let first = payload
                        .lines()
                        .find(|line| !line.trim().is_empty())
                        .unwrap_or("");
                    let mut preview: String = first.chars().take(160).collect();
                    if first.chars().count() > 160 {
                        preview.push('…');
                    }
                    content = content.push(text(preview).size(13));
                }
                content = content.push(action(
                    format!("message-detail-{}", message.id),
                    if expanded {
                        "Hide question"
                    } else {
                        "Show question"
                    },
                    Some(Message::QuestionDetails(message.id.clone())),
                    false,
                ));
            } else {
                content = content.push(text(payload).size(13));
            }
            lines = lines.push(
                container(
                    row![
                        small(ago(Utc::now(), message.sent_at), c).width(64),
                        content
                    ]
                    .spacing(10),
                )
                .padding([8, 10])
                .id(format!("notification-message-{}", message.id)),
            );
        }
        container(lines)
            .width(Fill)
            .style(move |_| c.surface(c.raised, false))
            .into()
    }

    fn channels_view(&self, c: Colors) -> Element<'_, Message> {
        let selected = self.shell.catalog.selected().map(|e| e.project.id());
        let (messages, _) = by_room(self.inbox.iter().chain(&self.sent_channels));
        let mut list = column![note(
            "Messages for you and sends from this window · partial history",
            c
        )]
        .spacing(14);
        let mut count = 0;
        for channel in self
            .channels
            .iter()
            .filter(|ch| Some(&ch.project) == selected.as_ref())
        {
            count += 1;
            let id = channel.id.to_string();
            let open = channel.is_open();
            let mut body = column![
                row![
                    heading(channel.title(), 17).width(Fill),
                    pill(
                        if open { "Open" } else { "Closed" },
                        if open { alpha(c.green, 0.16) } else { c.raised },
                        if open { c.green } else { c.muted },
                        c
                    )
                ]
                .spacing(10)
                .align_y(Center),
                small(
                    format!(
                        "{} members · {}",
                        channel.members.len(),
                        channel
                            .members
                            .iter()
                            .map(|id| self.name_of(id.as_str()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    c
                )
            ]
            .spacing(6);
            for review in &channel.reviews {
                body = body.push(
                    text(format!(
                        "{:?} by {} on {}: {}",
                        review.verdict, review.by_name, review.of_name, review.note
                    ))
                    .size(14),
                );
            }
            if let Some(resolution) = &channel.resolution {
                body = body.push(note(resolution.clone(), c));
            }
            match messages.get(&id) {
                Some(queued) if !queued.is_empty() => {
                    body =
                        body.push(self.transcript(self.recent_window(queued, 20).into_iter(), c));
                }
                _ => body = body.push(note("No messages to show in this channel yet.", c)),
            }
            body = body.push(action(
                format!("reply-channel-{id}"),
                "Write to channel",
                self.connected
                    .is_ok()
                    .then_some(Message::ChannelTarget(id.clone())),
                self.shell.channel_target.as_deref() == Some(id.as_str()),
            ));
            if self.shell.channel_target.as_deref() == Some(id.as_str()) {
                let draft = self
                    .shell
                    .channel_drafts
                    .get(&id)
                    .cloned()
                    .unwrap_or_default();
                if let Some(error) = &draft.error {
                    body = body.push(text(error.clone()).size(13).color(c.amber));
                }
                body = body.push(
                    row![
                        input(
                            "channel-message",
                            "Message",
                            &draft.text,
                            Message::ChannelDraft,
                        ),
                        primary(
                            "send-channel",
                            if draft.sending.is_some() {
                                "Sending…"
                            } else {
                                "Send message"
                            },
                            (draft.sending.is_none()
                                && self.connected.is_ok()
                                && !draft.text.trim().is_empty())
                            .then_some(Message::SendChannel),
                        )
                    ]
                    .spacing(8)
                    .align_y(Center),
                );
            }
            list = list.push(
                container(card(body.spacing(10), c)).id(format!("notification-channel-{id}")),
            );
        }
        if count == 0 {
            list = list.push(empty(
                "No channels yet",
                "Agents open channels here when they coordinate on a task.",
                None,
                c,
            ));
        }
        list.into()
    }

    fn journal_view(&self, c: Colors) -> Element<'_, Message> {
        let list = column![].spacing(12);
        if self.journal.is_empty() {
            return list
                .push(empty(
                    "No activity recorded yet",
                    "Commits, joins, leases and notes from this project will appear here.",
                    None,
                    c,
                ))
                .into();
        }
        let mut rows = column![].spacing(0);
        let total = self.journal.len();
        for (index, entry) in self.journal.iter().rev().enumerate() {
            rows = rows.push(
                container(
                    row![
                        column![
                            Space::new().height(5),
                            dot(if index == 0 { c.accent } else { c.faint }, 7.0, c)
                        ],
                        column![
                            text(entry.line()).size(14),
                            small(format!("{} · #{}", ago(Utc::now(), entry.at), entry.seq), c)
                        ]
                        .spacing(3)
                        .width(Fill)
                    ]
                    .spacing(12),
                )
                .padding([10, 12]),
            );
            if index + 1 < total {
                rows = rows.push(container(rule(c)).padding([0, 12]));
            }
        }
        list.push(panel(
            column![
                pane_header(
                    "Activity",
                    format!(
                        "{} of the latest {JOURNAL_WINDOW} · earlier entries stay in the journal",
                        self.journal.len()
                    ),
                    c
                ),
                rows
            ],
            c,
        ))
        .into()
    }

    fn coordination(&self, c: Colors) -> Element<'_, Message> {
        let mut list = column![
            heading("Resource coordination", 18),
            note(
                "Leases describe cooperative access reported to AgentDocker.",
                c
            )
        ]
        .spacing(12);
        let mut count = 0;
        for lease in &self.leases {
            let holder = self.agents.iter().find(|a| a.id == lease.holder);
            if !holder.is_some_and(|a| self.has_project(a.project.as_ref())) {
                continue;
            }
            count += 1;
            let now = Utc::now();
            let left_secs = (lease.expires_at - now).num_seconds();
            let left = remaining_fraction(lease.acquired_at, lease.expires_at, now);
            let mut body = column![
                row![
                    heading(lease.resource.to_string(), 15).width(Fill),
                    pill(format!("{:?}", lease.mode), c.accent_soft, c.accent_ink, c)
                ]
                .spacing(10)
                .align_y(Center),
                row![
                    small(self.name_of(lease.holder.as_str()), c).width(Fill),
                    small(
                        if left_secs > 0 {
                            format!("expires in {}", super::span(left_secs))
                        } else {
                            "expired".to_owned()
                        },
                        c
                    )
                ]
                .spacing(10),
                meter(left, if left < 0.2 { c.amber } else { c.accent }, c),
            ]
            .spacing(8);
            if let Some(why) = &lease.note {
                body = body.push(note(why.clone(), c));
            }
            list = list.push(card(body, c));
        }
        if count == 0 {
            list = list.push(empty(
                "No leases held",
                "When an agent claims a file or resource in this project it appears here.",
                None,
                c,
            ));
        }
        list.into()
    }

    fn terminal_view(&self, c: Colors) -> Element<'_, Message> {
        let Some(terminal) = &self.terminal else {
            return empty(
                "No terminal open",
                "Select a managed session and open its terminal.",
                None,
                c,
            );
        };
        let ended = matches!(terminal.status(), Status::Ended(_));
        let mut pane = column![
            row![
                dot(if ended { c.faint } else { c.green }, 8.0, c),
                heading(self.name_of(&terminal.agent), 16).width(Fill),
                pill(
                    if ended {
                        "Ended"
                    } else if terminal.scrolled_back() {
                        "Scrolled back"
                    } else {
                        "Live"
                    },
                    if ended {
                        c.raised
                    } else {
                        alpha(c.green, 0.16)
                    },
                    if ended { c.muted } else { c.green },
                    c
                ),
                action("detach-terminal", "Detach", Some(Message::Detach), false)
            ]
            .spacing(10)
            .align_y(Center),
        ]
        .spacing(10);
        if let Status::Ended(reason) = terminal.status() {
            pane = pane.push(text(reason).size(13).color(c.amber));
        }
        if let Some(reason) = terminal.input_notice {
            pane = pane.push(
                row![
                    text(reason).size(13).color(c.amber),
                    action(
                        "dismiss-terminal-input",
                        "Dismiss",
                        Some(Message::TerminalDismiss),
                        false
                    )
                ]
                .spacing(8)
                .align_y(Center),
            );
        }
        if terminal.scrolled_back() {
            pane = pane.push(action(
                "terminal-live",
                "Return to live output",
                Some(Message::TerminalScroll(-2000)),
                false,
            ));
        }
        let palette = self.settings.palette();
        let ground: iced::Color = palette.ground.into();
        pane = pane.push(
            container(crate::terminal::Display {
                terminal,
                palette,
                size: self.settings.terminal_size,
                height: (self.shell.height / self.scale_factor() - 300.0).max(240.0),
            })
            .padding(6)
            .style(move |_| c.surface(ground, false)),
        );
        column![
            container(pane)
                .padding(10)
                .width(Fill)
                .style(move |_| c.card_style()),
            small("F6 leaves terminal focus · Ctrl+] detaches", c)
        ]
        .spacing(8)
        .into()
    }

    fn console_view(&self, c: Colors) -> Element<'_, Message> {
        let palette = self.settings.palette();
        let ground: iced::Color = palette.ground.into();
        let ink: iced::Color = palette.text.into();
        column![
            heading("AgentDocker commands", 18),
            note("Run an AgentDocker command in this project.", c),
            row![
                input(
                    "console-command",
                    "agentdocker command",
                    &self.console_input,
                    Message::ConsoleInput
                ),
                primary(
                    "run-command",
                    if self.console_running > 0 {
                        "Run another command"
                    } else {
                        "Run command"
                    },
                    (!self.console_input.trim().is_empty()).then_some(Message::RunConsole),
                ),
            ]
            .spacing(8)
            .align_y(Center),
            row![
                action(
                    "previous-command",
                    "Previous",
                    Some(Message::Recall(true)),
                    false
                ),
                action("next-command", "Next", Some(Message::Recall(false)), false)
            ]
            .spacing(6),
            container(
                text(self.console_output.clone())
                    .font(Font::MONOSPACE)
                    .size(self.settings.terminal_size)
                    .color(ink)
            )
            .padding(14)
            .width(Fill)
            .style(move |_| c.surface(ground, true))
        ]
        .spacing(12)
        .into()
    }

    fn connections(&self, c: Colors) -> Element<'_, Message> {
        let mut list = column![].spacing(14);
        if self.setup_busy {
            list = list.push(note("Checking…", c));
        }
        if let Some(error) = &self.shell.setup_error {
            list = list.push(text(error.clone()).size(13).color(c.amber));
        }
        if let Some(plan) = &self.setup_plan {
            list = list.push(self.setup_view(plan, c));
        }
        if let Some(health) = &self.setup_health {
            list = list.push(self.health_view(health, c));
        }
        let mut runtimes: Vec<_> = self
            .runtimes
            .iter()
            .filter(|r| self.shell.other_tools || r.installed())
            .collect();
        runtimes.sort_by_key(|r| !r.installed());
        if runtimes.is_empty() {
            list = list.push(empty(
                "No supported tools found",
                "Install Claude Code or Codex, then come back here.",
                None,
                c,
            ));
        }
        for runtime in runtimes {
            let expanded = self.shell.connection_details.as_deref() == Some(runtime.name.as_str());
            let installed = runtime.installed();
            let reporting = self.tool_reports(&runtime.name);
            let missing = installed
                && (runtime.mcp == agentdocker_core::runtime::Wiring::Missing
                    || runtime.hooks == agentdocker_core::runtime::Wiring::Missing);
            // One word a person can act on. Green only for a tool that has
            // actually reported through its connection; configuration on
            // disk is a promise, not proof.
            let (mark, word) = if !installed {
                (c.faint, "Not installed")
            } else if reporting {
                (c.green, "Connected")
            } else if missing {
                (c.amber, "Needs setup")
            } else {
                (c.cyan, "Configured, waiting for its first session")
            };
            let mut actions = row![
                dot(mark, 9.0, c),
                column![
                    heading(runtime.label.clone(), 16),
                    small(
                        format!(
                            "{word}{}",
                            runtime
                                .version
                                .as_deref()
                                .map(|v| format!(" · {v}"))
                                .unwrap_or_default()
                        ),
                        c
                    )
                ]
                .spacing(2)
                .width(Fill)
            ]
            .spacing(12)
            .align_y(Center);
            if missing && !reporting {
                actions = actions.push(primary(
                    format!("setup-{}", runtime.name),
                    "Set up",
                    (!self.setup_busy).then_some(Message::Setup(vec![
                        runtime.name.clone(),
                        "--preview".into(),
                    ])),
                ));
            }
            actions = actions.push(action(
                format!("connection-details-{}", runtime.name),
                if expanded { "Hide details" } else { "Details" },
                Some(Message::ConnectionDetails(runtime.name.clone())),
                expanded,
            ));
            let mut details = column![actions].spacing(10);
            if expanded {
                let wiring = |w: agentdocker_core::runtime::Wiring| match w {
                    agentdocker_core::runtime::Wiring::Wired => "configured",
                    agentdocker_core::runtime::Wiring::Unverified => "configured, unverified",
                    agentdocker_core::runtime::Wiring::Missing => "not configured",
                    agentdocker_core::runtime::Wiring::Unsupported => "not available",
                };
                let mut facts = column![
                    kv("Vendor", runtime.vendor.to_string(), c),
                    kv(
                        "Version",
                        runtime.version.as_deref().unwrap_or("Version unknown"),
                        c
                    ),
                    kv(
                        "Command",
                        runtime
                            .cli
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "No command-line tool found".into()),
                        c
                    ),
                    kv("Tools (MCP)", wiring(runtime.mcp), c),
                    kv("Live activity (hooks)", wiring(runtime.hooks), c),
                ]
                .spacing(6);
                for app in &runtime.apps {
                    facts = facts.push(kv("Application", app.label.clone(), c));
                }
                if installed && !missing && !reporting {
                    facts = facts.push(note(
                        "Configured. It shows as connected the first time a session of this tool \
                         reports in.",
                        c,
                    ));
                }
                facts = facts.push(
                    row![
                        action(
                            format!("setup-review-{}", runtime.name),
                            "Review setup",
                            (!self.setup_busy).then_some(Message::Setup(vec![
                                runtime.name.clone(),
                                "--preview".into(),
                            ])),
                            false
                        ),
                        action(
                            "connection-health",
                            "Check connections",
                            (!self.setup_busy).then_some(Message::Setup(vec!["--health".into()])),
                            false
                        ),
                        action(
                            "saved-setups",
                            "Setup history",
                            (!self.setup_busy).then_some(Message::Setup(vec!["--list".into()])),
                            false
                        )
                    ]
                    .spacing(6)
                    .wrap(),
                );
                details = details.push(rule(c)).push(facts);
            }
            list = list.push(card(details, c));
        }
        if !self.discovered.is_empty() {
            list = list.push(action(
                "register-all-discovered",
                format!("Connect all {} running sessions", self.discovered.len()),
                self.connected.is_ok().then_some(Message::AdoptAll),
                false,
            ));
        }
        for plan in &self.setup_history {
            let id = value(plan, "id");
            list = list.push(action(
                format!("saved-setup-{id}"),
                format!("{} · {id}", value(plan, "phase")),
                (!self.setup_busy).then_some(Message::Setup(vec!["--show".into(), id])),
                false,
            ));
        }
        let other_count = self.runtimes.iter().filter(|r| !r.installed()).count();
        if other_count > 0 {
            list = list.push(action(
                "other-tools",
                if self.shell.other_tools {
                    "Hide other tools".into()
                } else {
                    format!("Other supported tools ({other_count})")
                },
                Some(Message::OtherTools),
                false,
            ));
        }
        list.into()
    }

    /// The connection check, in words: one line per installed tool, and a
    /// way to put it away.
    fn health_view(&self, health: &serde_json::Value, c: Colors) -> Element<'_, Message> {
        let mut report = column![
            row![
                heading("Connection check", 16).width(Fill),
                action("close-health", "Close", Some(Message::SetupClose), false)
            ]
            .spacing(10)
            .align_y(Center),
            small(value(health, "daemon"), c)
        ]
        .spacing(8);
        let installed: BTreeSet<&str> = self
            .runtimes
            .iter()
            .filter(|r| r.installed())
            .map(|r| r.name.as_str())
            .collect();
        for runtime in health["runtimes"].as_array().into_iter().flatten() {
            let name = value(runtime, "name");
            if !installed.contains(name.as_str()) {
                continue;
            }
            let label = self
                .runtimes
                .iter()
                .find(|r| r.name == name)
                .map(|r| r.label.clone())
                .unwrap_or(name);
            let checks = runtime["checks"].as_array().into_iter().flatten();
            let mut problems = Vec::new();
            for check in checks {
                let status = value(check, "status");
                if matches!(
                    status.as_str(),
                    "ok" | "executable_available" | "unsupported"
                ) {
                    continue;
                }
                problems.push(format!(
                    "{}: {}",
                    value(check, "channel"),
                    value(check, "detail")
                ));
            }
            let ok = problems.is_empty();
            let mut line = column![
                row![
                    dot(if ok { c.green } else { c.amber }, 7.0, c),
                    text(label).size(14).width(Fill),
                    small(if ok { "Ready" } else { "Needs attention" }, c)
                ]
                .spacing(10)
                .align_y(Center)
            ]
            .spacing(2);
            for problem in problems {
                line = line.push(container(small(problem, c)).padding([0, 17]));
            }
            report = report.push(line);
        }
        card(report, c)
    }

    /// One setup plan, said plainly: what will change, one button to do it,
    /// the technical record behind Details.
    fn setup_view(&self, plan: &serde_json::Value, c: Colors) -> Element<'_, Message> {
        let plan_id = plan["id"].as_str().filter(|id| !id.is_empty());
        let id = plan_id.unwrap_or("unknown").to_owned();
        let phase = value(plan, "phase");
        let changes = plan["changes"].as_array();
        let tool = changes
            .into_iter()
            .flatten()
            .next()
            .map(|change| value(change, "runtime"))
            .and_then(|name| self.runtimes.iter().find(|r| r.name == name))
            .map(|r| r.label.clone());
        let title_text = match (&tool, phase.as_str()) {
            (Some(tool), "applied") => format!("{tool} connected"),
            (Some(tool), "undone") => format!("{tool} setup undone"),
            (Some(tool), _) => format!("Connect {tool}"),
            (None, "applied") => "Connected".to_owned(),
            (None, _) => "Nothing to connect".to_owned(),
        };
        let mut body = column![
            row![
                heading(title_text, 18).width(Fill),
                action(
                    "close-setup",
                    "Close",
                    (!self.setup_busy).then_some(Message::SetupClose),
                    false
                )
            ]
            .spacing(10)
            .align_y(Center)
        ]
        .spacing(10);
        let mut any = false;
        for change in changes.into_iter().flatten() {
            any = true;
            let what = match value(change, "channel").as_str() {
                "mcp" => "Tools (MCP)".to_owned(),
                "activity hooks" | "hooks" => "Live activity (hooks)".to_owned(),
                other => other.to_owned(),
            };
            body = body.push(
                row![
                    dot(c.accent, 7.0, c),
                    text(format!("{what}: {}", value(change, "action"))).size(14)
                ]
                .spacing(10)
                .align_y(Center),
            );
        }
        if !any {
            body = body.push(note("Everything this tool needs is already in place.", c));
        }
        let applicable = !self.setup_busy
            && matches!(phase.as_str(), "prepared" | "applying")
            && any
            && plan_id.is_some();
        let expanded = self.shell.connection_details.as_deref() == Some("setup-plan");
        let mut buttons = row![].spacing(6);
        if applicable {
            buttons = buttons.push(primary(
                "apply-setup",
                "Connect",
                Some(Message::Setup(vec!["--apply".into(), id.clone()])),
            ));
        }
        if phase == "applied" && plan_id.is_some() {
            buttons = buttons.push(action(
                "undo-setup",
                "Undo",
                (!self.setup_busy).then_some(Message::Setup(vec!["--undo".into(), id.clone()])),
                false,
            ));
        }
        buttons = buttons.push(action(
            "setup-details",
            if expanded { "Hide details" } else { "Details" },
            Some(Message::ConnectionDetails("setup-plan".into())),
            expanded,
        ));
        body = body.push(buttons.wrap());
        if expanded {
            let mut facts =
                column![kv("Plan", id.clone(), c), kv("State", phase.clone(), c)].spacing(6);
            if let Some(executable) = plan["executable"].as_str() {
                facts = facts.push(kv("Command", executable, c));
            }
            for change in changes.into_iter().flatten() {
                facts = facts.push(kv("File", value(change, "path"), c));
            }
            for note_text in plan["notes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s.as_str())
            {
                facts = facts.push(small(note_text, c));
            }
            body = body.push(rule(c)).push(facts);
        }
        card(body, c)
    }

    fn settings_view(&self, c: Colors) -> Element<'_, Message> {
        let mut palettes = row![].spacing(6);
        for palette in crate::theme::PALETTES {
            let ground: iced::Color = palette.ground.into();
            let accent: iced::Color = palette.accent.into();
            let content = row![
                container(dot(accent, 6.0, c))
                    .width(16)
                    .height(16)
                    .center_x(16)
                    .center_y(16)
                    .style(move |_| container::Style {
                        background: Some(ground.into()),
                        border: iced::Border {
                            color: c.line,
                            width: 1.0,
                            radius: 4.0.into(),
                        },
                        ..Default::default()
                    }),
                text(palette.name).size(14)
            ]
            .spacing(8)
            .align_y(Center);
            palettes = palettes.push(custom(
                format!("palette-{}", palette.name),
                palette.name,
                content,
                Some(Message::Palette(palette.name.into())),
                self.settings.palette == palette.name,
                Kind::Secondary,
                [7, 11],
            ));
        }
        let appearance = card(
            column![
                heading("Appearance", 18),
                row![
                    small("Theme", c).width(110),
                    segmented(
                        vec![
                            segment(
                                "light-theme",
                                "Light",
                                Some(Message::Dark(false)),
                                !self.shell.catalog.dark,
                            ),
                            segment(
                                "dark-theme",
                                "Dark",
                                Some(Message::Dark(true)),
                                self.shell.catalog.dark,
                            ),
                        ],
                        c
                    )
                ]
                .spacing(6)
                .align_y(Center),
                row![
                    small("Text size", c).width(110),
                    action(
                        "smaller-ui",
                        "Smaller text",
                        Some(Message::TextSize(self.settings.text_size - 1.0)),
                        false
                    ),
                    text(format!("{:.0} pt", self.settings.text_size)).size(13),
                    action(
                        "larger-ui",
                        "Larger text",
                        Some(Message::TextSize(self.settings.text_size + 1.0)),
                        false
                    )
                ]
                .spacing(6)
                .align_y(Center),
                row![
                    small("Rows", c).width(110),
                    action(
                        "roomy-rows",
                        if self.settings.roomy {
                            "Use compact rows"
                        } else {
                            "Use roomier rows"
                        },
                        Some(Message::Roomy(!self.settings.roomy)),
                        self.settings.roomy
                    )
                ]
                .spacing(6)
                .align_y(Center)
            ]
            .spacing(12),
            c,
        );
        let terminal = card(
            column![
                heading("Terminal", 18),
                row![
                    small("Text size", c).width(110),
                    action(
                        "smaller-terminal",
                        "Smaller terminal text",
                        Some(Message::TerminalSize(self.settings.terminal_size - 1.0)),
                        false
                    ),
                    text(format!("{:.0} pt", self.settings.terminal_size)).size(13),
                    action(
                        "larger-terminal",
                        "Larger terminal text",
                        Some(Message::TerminalSize(self.settings.terminal_size + 1.0)),
                        false
                    )
                ]
                .spacing(6)
                .align_y(Center),
                column![small("Palette", c), palettes.wrap()].spacing(8)
            ]
            .spacing(12),
            c,
        );
        let keyboard = card(
            column![
                heading("Keyboard and accessibility", 18),
                note(
                    "Tab / Shift+Tab move through controls. Enter / Space activate focused buttons. Command/Ctrl+1–4 switch sections. F6 leaves terminal input. Escape closes session details or a draft launch form. Text controls support native input methods.",
                    c
                )
            ]
            .spacing(10),
            c,
        );
        let mut installation = column![
            heading("Installation", 18),
            action(
                "automatic-update-checks",
                if self.shell.catalog.updates.enabled {
                    "Daily update checks: on"
                } else {
                    "Daily update checks: off"
                },
                self.shell.save_enabled.then_some(Message::AutomaticUpdates(
                    !self.shell.catalog.updates.enabled
                )),
                self.shell.catalog.updates.enabled,
            ),
            note("Preview and apply installs, rollbacks and cleanup.", c),
            action(
                "installation",
                "Manage installation and retained versions",
                Some(Message::Navigate(Screen::Desktop)),
                false
            )
        ]
        .spacing(10);
        if self.desktop.update_check_error {
            installation = installation.push(small(
                "Couldn’t check for updates. Try again in Installation.",
                c,
            ));
        }
        let installation = card(installation, c);
        let diagnostics = card(
            column![
                heading("Diagnostics", 18),
                kv("Local daemon", self.socket.clone(), c),
                kv(
                    "Preferences",
                    self.home.join("workspace.json").display().to_string(),
                    c
                )
            ]
            .spacing(8),
            c,
        );
        column![appearance, terminal, keyboard, installation, diagnostics]
            .spacing(14)
            .into()
    }

    fn installation_view(&self, c: Colors) -> Element<'_, Message> {
        let p = &self.desktop;
        let mut body = column![
            heading("Desktop installation", 18),
            note(
                "Preview an install, rollback, or cleanup before applying it. Activation takes effect on the next app launch; running agents continue.",
                c
            ),
            input(
                "desktop-source",
                "Application bundle or extracted package",
                &p.source,
                Message::DesktopSource
            ),
            action(
                "desktop-use-current",
                "Use this application",
                (!p.busy).then_some(Message::DesktopUseCurrent),
                false
            ),
            input(
                "desktop-prefix",
                "Installation prefix (empty uses your home)",
                &p.prefix,
                Message::DesktopPrefix
            )
        ]
        .spacing(12);
        if cfg!(target_os = "macos") {
            body = body.push(action(
                "desktop-local",
                if p.local_preview {
                    "Local preview signatures allowed"
                } else {
                    "Allow locally signed preview builds"
                },
                (!p.busy).then_some(Message::DesktopLocal(!p.local_preview)),
                p.local_preview,
            ));
        }
        let updates = row![
            primary(
                "desktop-update-check",
                "Check for updates",
                p.preview("update-check")
                    .map(|_| Message::DesktopPreview("update-check".into())),
            ),
            action(
                "desktop-update",
                match p.update_available() {
                    Some(version) => format!("Download and preview {version}"),
                    None => "Download and preview update".to_owned(),
                },
                p.preview("update")
                    .map(|_| Message::DesktopPreview("update".into())),
                false,
            )
        ]
        .spacing(6)
        .align_y(Center)
        .wrap();
        body = body.push(column![eyebrow("Updates", c), updates].spacing(8));
        let mut operations = row![].spacing(6);
        for (operation, label) in [
            ("status", "Show installed versions"),
            ("install", "Preview installation"),
            ("rollback", "Preview rollback"),
            ("prune", "Preview cleanup"),
            ("uninstall", "Preview removal"),
        ] {
            operations = operations.push(action(
                format!("desktop-{operation}"),
                label,
                p.preview(operation)
                    .map(|_| Message::DesktopPreview(operation.into())),
                false,
            ));
        }
        body = body.push(operations.wrap());
        if p.busy {
            body = body.push(note("Verifying installation…", c));
        }
        if let Some(error) = &p.error {
            body = body.push(text(error.clone()).size(13).color(c.amber));
        }
        let mut screen = column![card(body, c)].spacing(14);
        if let Some(update) = p
            .update
            .as_ref()
            .or_else(|| p.report.as_ref().and_then(|r| r.get("update")))
        {
            let available = update["available"]["version"].as_str().unwrap_or("unknown");
            let installed = update["installed_version"]
                .as_str()
                .or(update["running_version"].as_str())
                .unwrap_or("unknown");
            let mut facts = column![
                heading(
                    if update["update_available"] == true {
                        format!("Version {available} is available")
                    } else {
                        "You have the newest release".to_owned()
                    },
                    16
                ),
                kv("Installed", installed, c),
                kv("Available", available, c),
                kv("Channel", value(update, "channel"), c),
                kv("Daemon", value(update, "daemon"), c),
            ]
            .spacing(6);
            if update["state_schema_change"] == true {
                facts = facts.push(note(
                    "This release changes the daemon's state schema: after installing, rollback needs a matching state backup.",
                    c,
                ));
            }
            screen = screen.push(card(facts, c));
        }
        if let Some(report) = &p.report {
            let mut details = column![heading(
                if report["preview"] == true {
                    "Review this plan"
                } else {
                    "Installation report"
                },
                16
            )]
            .spacing(8);
            if let Some(maintenance) = report.get("maintenance") {
                for path in maintenance["remove"].as_array().into_iter().flatten() {
                    details = details.push(kv("Remove", path.as_str().unwrap_or("unknown"), c));
                }
                for entry in maintenance["retained"].as_array().into_iter().flatten() {
                    details = details.push(kv(
                        "Keep",
                        format!("{} · {}", value(entry, "path"), value(entry, "reason")),
                        c,
                    ));
                }
            } else if let Some(installation) = report.get("installation") {
                if installation.is_null() {
                    details = details.push(note("No managed installation at this prefix.", c));
                } else {
                    for (key, label) in [("current", "Active"), ("previous", "Previous")] {
                        if !installation[key].is_null() {
                            details = details.push(kv(
                                label,
                                format!(
                                    "{} · source {}",
                                    value(&installation[key], "version"),
                                    value(&installation[key], "source_commit")
                                ),
                                c,
                            ));
                        }
                    }
                }
            } else {
                details = details.push(kv(
                    "Release",
                    format!(
                        "{} · source {}",
                        value(&report["candidate"], "version"),
                        value(&report["candidate"], "source_commit")
                    ),
                    c,
                ));
                for key in ["source", "application", "bin", "versions"] {
                    if let Some(path) = report[key].as_str() {
                        details = details.push(kv(key, path, c));
                    }
                }
            }
            if report["preview"] == true {
                details = details.push(primary(
                    "desktop-apply",
                    "Apply this reviewed plan",
                    p.apply().map(|_| Message::DesktopApply),
                ));
            }
            screen = screen.push(card(details, c));
        }
        screen.into()
    }
}

/// Keep full question bodies in the review screen, with a bounded first line here.
fn compact_question(value: &str) -> String {
    let first = value.lines().next().unwrap_or_default();
    let mut preview: String = first.chars().take(80).collect();
    if first.chars().count() > 80 || value.lines().count() > 1 {
        preview.push('…');
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::{remaining_fraction, spoken_payload};
    use chrono::{Duration, Utc};
    use serde_json::json;

    #[test]
    fn a_window_drains_from_one_to_zero_and_never_beyond() {
        let start = Utc::now();
        let end = start + Duration::seconds(100);
        assert_eq!(remaining_fraction(start, end, start), 1.0);
        assert!((remaining_fraction(start, end, start + Duration::seconds(50)) - 0.5).abs() < 1e-3);
        assert_eq!(remaining_fraction(start, end, end), 0.0);
        assert_eq!(
            remaining_fraction(start, end, end + Duration::seconds(5)),
            0.0
        );
        assert_eq!(
            remaining_fraction(start, end, start - Duration::seconds(5)),
            1.0
        );
        // A window with no length, or one that ends before it starts, is over.
        assert_eq!(remaining_fraction(start, start, start), 0.0);
        assert_eq!(remaining_fraction(end, start, start), 0.0);
    }

    #[test]
    fn a_message_shows_its_text_and_only_textless_payloads_show_json() {
        assert_eq!(spoken_payload(&json!("plain")), "plain");
        assert_eq!(
            spoken_payload(&json!({"text": "hello", "channel": "c1"})),
            "hello"
        );
        assert!(spoken_payload(&json!({"verdict": "approve"})).contains("\"verdict\""));
    }
}
