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
    Kind, block_button, button as action, composer, custom, danger, input, input_submitting,
    primary, segment, tab,
};
use iced::{
    Center, Element, Fill, Font,
    widget::{Space, column, container, row, scrollable, text},
};

pub(super) fn heading<'a>(value: impl Into<String>, size: u32) -> iced::widget::Text<'a> {
    text(value.into())
        .size(size)
        .font(weight(iced::font::Weight::Semibold))
}
fn title<'a>(value: impl Into<String>, size: u32) -> iced::widget::Text<'a> {
    text(value.into())
        .size(size)
        .font(weight(iced::font::Weight::Semibold))
}
pub(super) fn note<'a>(value: impl Into<String>, c: Colors) -> iced::widget::Text<'a> {
    text(value.into()).size(13).color(c.muted)
}
/// The typed references beside a card, a message or a hand-off, one per
/// row: the kind as a quiet pill, the target as text to read, and the
/// note after it. Nothing is opened or copied from here — a path or a
/// pull request is the reader's to open with their own tools; what the
/// row does is say what kind of thing it is without prose.
pub(super) fn links<'a>(links: &[agentdocker_core::Link], c: Colors) -> Element<'a, Message> {
    let mut rows = column![].spacing(3).width(Fill);
    for link in links {
        let mut line = row![
            pill(link.kind.as_str().to_owned(), c.raised, c.muted, c),
            text(link.target.clone())
                .size(12)
                .color(c.accent)
                .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
        ]
        .spacing(6)
        .align_y(Center);
        if let Some(note) = &link.note {
            line = line.push(small(note.clone(), c));
        }
        rows = rows.push(line);
    }
    rows.into()
}

pub(super) fn small<'a>(value: impl Into<String>, c: Colors) -> iced::widget::Text<'a> {
    text(value.into()).size(12).color(c.muted)
}
/// A section label: short, quiet, set in capitals.
pub(super) fn eyebrow<'a>(value: impl Into<String>, c: Colors) -> iced::widget::Text<'a> {
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
pub(super) fn panel<'a>(
    content: impl Into<Element<'a, Message>>,
    c: Colors,
) -> Element<'a, Message> {
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
pub(super) fn pill<'a>(
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
pub(super) fn dot<'a>(fill: iced::Color, size: f32, c: Colors) -> Element<'a, Message> {
    container(Space::new().width(size).height(size))
        .style(move |_| c.dot(fill))
        .into()
}
/// A project's mark: its initial on its own tint. The tint comes from
/// the project's identity, so a repository keeps its colour across
/// clones, machines and themes, and two projects side by side are told
/// apart before their names are read.
pub(super) fn monogram<'a>(name: &str, seed: &str, size: f32, c: Colors) -> Element<'a, Message> {
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
pub(super) fn segmented<'a>(choices: Vec<Element<'a, Message>>, c: Colors) -> Element<'a, Message> {
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
pub(super) fn rule<'a>(c: Colors) -> Element<'a, Message> {
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
pub(super) fn first_line(text: &str, limit: usize) -> String {
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
pub(super) fn spoken_payload(payload: &serde_json::Value) -> String {
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
pub(super) fn empty<'a>(
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
            // A session started in `/` or the home folder has a project
            // only in name; it is one of the Other sessions.
            None if self.shell.catalog.unassigned => {
                project.is_none_or(|p| self.shell.catalog.broad_unpinned(&p.root))
            }
            None => true,
        }
    }
    /// The home view: no project chosen, everything shown.
    pub(super) fn all_projects(&self) -> bool {
        self.selected_root().is_none() && !self.shell.catalog.unassigned
    }
    pub(super) fn narrow(&self) -> bool {
        self.shell.width / self.scale_factor() < 900.0
    }
    fn in_project(&self) -> bool {
        !matches!(
            self.screen,
            Screen::Questions | Screen::Runtimes | Screen::Settings | Screen::Desktop
        )
    }
    /// The footer while the daemon is older than this window: what is wrong,
    /// the one thing that fixes it, and what that costs before it is done.
    fn daemon_notice(&self, c: Colors) -> Option<Element<'_, Message>> {
        let version = self.daemon_behind_now()?;
        let connected = self.connected.is_ok();
        let row = if self.daemon_restarting() {
            row![
                dot(c.amber, 7.0, c),
                small("Restarting the background service…", c).width(Fill),
            ]
        } else if self.shell.confirm_daemon_restart {
            let (stop, back) = self.restart_cost();
            let cost = match (stop, back) {
                (0, _) => "Sessions keep running and reconnect on their own.".to_owned(),
                (n, 0) => format!(
                    "{n} session{} AgentDocker started will stop. Sessions started in a terminal keep running.",
                    if n == 1 { "" } else { "s" }
                ),
                (n, m) => format!(
                    "{n} session{} AgentDocker started will stop and {m} will start again. Sessions started in a terminal keep running.",
                    if n == 1 { "" } else { "s" }
                ),
            };
            row![
                dot(c.amber, 7.0, c),
                small(format!("Restart the background service now? {cost}"), c).width(Fill),
                primary(
                    "daemon-restart-confirm",
                    "Restart",
                    connected.then_some(Message::RestartDaemon),
                ),
                action(
                    "daemon-restart-cancel",
                    "Cancel",
                    Some(Message::ConfirmDaemonRestart(false)),
                    false,
                ),
            ]
        } else {
            row![
                dot(c.amber, 7.0, c),
                small(
                    format!(
                        "The background service is running an older version ({version}) than this app ({}). Restart it to finish updating.",
                        env!("CARGO_PKG_VERSION")
                    ),
                    c
                )
                .width(Fill),
                action(
                    "daemon-restart",
                    "Restart background service…",
                    connected.then_some(Message::ConfirmDaemonRestart(true)),
                    false,
                ),
            ]
        };
        Some(
            container(row.spacing(8).align_y(Center))
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
                .into(),
        )
    }

    /// The words for what an agent is doing, live or finished.
    pub(super) fn activity_label(&self, agent: &AgentRecord) -> String {
        let id = agent.id.to_string();
        if self.needs_input(&id) {
            "needs input".to_owned()
        } else if let Some((_, state)) = agentdocker_core::provider_block(agent, &self.agents) {
            state
                .issue
                .as_ref()
                .expect("blocked")
                .kind
                .label()
                .to_owned()
        } else if self.delivery_paused(agent) && agent.status.is_live() {
            "not receiving messages".to_owned()
        } else if self.ended_with_undelivered(agent) {
            format!("ended {}", undelivered_phrase(self.undelivered(agent)))
        } else if agent.status.is_live() {
            match self.activity.get(&id) {
                None | Some(Activity::Unknown) => "running".to_owned(),
                Some(activity) => activity.label().to_owned(),
            }
        } else if let agentdocker_core::AgentStatus::Failed { reason } = &agent.status {
            format!("failed: {reason}")
        } else {
            "ended".to_owned()
        }
    }
    /// The colour that goes with [`Self::activity_label`].
    fn activity_color(&self, agent: &AgentRecord, c: Colors) -> iced::Color {
        if self.needs_input(&agent.id.to_string()) || self.delivery_needs_you(agent) {
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

    /// Recent, generation-bound adapter contact; general activity, leases
    /// and saved configuration cannot establish a connection.
    fn tool_reports(&self, runtime: &str) -> bool {
        let now = Utc::now();
        self.connected.is_ok()
            && self.agents.iter().any(|a| {
                a.spec.runtime == runtime
                    && a.status.is_live()
                    && (a
                        .adapter_contacts
                        .values()
                        .any(|contact| contact.current_for(a.process_started_at, now))
                        || a.input_delivery.as_ref().is_some_and(|delivery| {
                            delivery.current_for(a.process_started_at, now)
                        }))
            })
    }

    pub(super) fn input_readiness(&self, agent: &AgentRecord) -> &'static str {
        if !agent.status.is_live() {
            return "Session ended";
        }
        if self.connected.is_err() {
            return "Readiness unavailable";
        }
        if let Some((_, state)) = agentdocker_core::provider_block(agent, &self.agents) {
            return state.issue.as_ref().expect("blocked").kind.label();
        }
        let readiness = agentdocker_core::InputReadiness::for_agent(agent, Utc::now());
        // A current receiver with words queued that no receipt covers is
        // waiting on the provider to take them; an earlier receipt does not
        // make them delivered. Limits, pauses and silence come first.
        if matches!(
            readiness,
            agentdocker_core::InputReadiness::Verified
                | agentdocker_core::InputReadiness::AwaitingFirstReceipt
        ) && self
            .awaiting_receipt
            .get(agent.id.as_str())
            .is_some_and(|count| *count > 0)
        {
            return "Sent · waiting for the agent to take it";
        }
        readiness.label()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let c = Colors::new(self.shell.catalog.dark);
        let narrow = self.narrow();
        // Wide, the rail and the page are two panes with a divider the
        // person drags (the width is kept, in pixels, with the workspace
        // preferences); narrow, the rail is a fixed strip.
        let workspace: Element<'_, Message> = if narrow {
            row![self.sidebar(c), self.page(c)].height(Fill).into()
        } else {
            iced::widget::pane_grid(&self.panes.shell, |_, slot, _| {
                iced::widget::pane_grid::Content::new(match slot {
                    super::panes::Slot::Rail => self.sidebar(c),
                    _ => self.page(c),
                })
            })
            .on_resize(8, |event| {
                Message::PaneResized(super::panes::Grid::Shell, event)
            })
            .style(move |_| split_style(c))
            .height(Fill)
            .into()
        };
        column![workspace, self.footer(c)].into()
    }

    /// The page beside the rail: the header, the tabs and the screen.
    fn page(&self, c: Colors) -> Element<'_, Message> {
        let in_project = self.in_project();
        let narrow = self.narrow();
        let title_text = if in_project {
            self.shell
                .catalog
                .selected()
                .map(|e| e.name())
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
                Screen::Questions if self.has_conversations() => "Messages",
                Screen::Questions => "Inbox",
                Screen::Runtimes => "Tools",
                _ => "Settings",
            }
            .into()
        };
        let mut heading_row = row![].spacing(10).align_y(Center);
        if in_project && let Some(entry) = self.shell.catalog.selected() {
            heading_row = heading_row.push(monogram(
                &entry.name(),
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
                    Screen::Questions if self.has_conversations() => {
                        "Channels, direct messages and what your agents were told"
                    }
                    Screen::Questions => "Questions and messages waiting for you",
                    Screen::Runtimes => "Connect and configure your agent tools",
                    _ => "Appearance, terminal, installation and diagnostics",
                },
                c,
            ));
        }
        let mut header = row![header_left].spacing(16).align_y(Center);
        // The project's hold and its one primary action stay in the header
        // on every project screen: they vanished on Board and History.
        if in_project
            && !narrow
            && let Some(pause) = self.pause_controls(c)
        {
            header = header.push(pause);
        }
        if in_project
            && !narrow
            && let Some(launch) = self.launch_button()
        {
            header = header.push(launch);
        }
        let mut content = column![header].spacing(18).width(Fill);
        let mut project_actions = row![].spacing(8);
        if in_project && self.shell.catalog.selected().is_some() {
            project_actions = project_actions.push(action(
                "project-terminal",
                "Open project terminal",
                (!self.shell.terminal_opening && self.shell.project_available != Some(false))
                    .then_some(Message::OpenProjectTerminal),
                false,
            ));
        }
        // Narrow, the hold has its own line under the header rather than
        // none: a pause is not a thing to lose with the width.
        if in_project
            && narrow
            && let Some(pause) = self.pause_controls(c)
        {
            project_actions = project_actions.push(pause);
        }
        if in_project
            && self.screen != Screen::Agents
            && narrow
            && let Some(launch) = self.launch_button()
        {
            project_actions = project_actions.push(launch);
        }
        if in_project && self.shell.catalog.selected().is_some() {
            content = content.push(project_actions.wrap());
        }
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
        if let Some(error) = &self.shell.drafts.error {
            let mut controls = row![].spacing(8);
            if self.shell.drafts.readable {
                controls = controls.push(action(
                    "retry-draft-save",
                    "Retry saving",
                    Some(Message::RetryDraftSave),
                    false,
                ));
            }
            if self.shell.drafts.close_blocked {
                controls = controls.push(action(
                    "close-without-drafts",
                    "Close without saving",
                    Some(Message::CloseWithoutDraftSave),
                    false,
                ));
            }
            content = content.push(attention(
                column![text(error.clone()).size(14).color(c.amber), controls].spacing(8),
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
                (Screen::Chat, "Chat", Icon::Channels),
                (Screen::Agents, "Agents", Icon::Sessions),
                (Screen::Board, "Board", Icon::Board),
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
            // Underlined only on one of its own screens: with its drawer open
            // over Agents, two tabs read as selected at once.
            let more_selected = matches!(
                self.screen,
                Screen::Journal
                    | Screen::Channels
                    | Screen::Leases
                    | Screen::Console
                    | Screen::Usage
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
                action(
                    "project-tab-Journal",
                    "History",
                    Some(Message::Navigate(Screen::Journal)),
                    self.screen == Screen::Journal
                ),
                action(
                    "project-tab-Channels",
                    if queued > 0 {
                        format!("Channels ({queued} waiting)")
                    } else {
                        "Channels".to_owned()
                    },
                    Some(Message::Navigate(Screen::Channels)),
                    self.screen == Screen::Channels
                ),
                action(
                    "project-tab-Leases",
                    "Files in use",
                    Some(Message::Navigate(Screen::Leases)),
                    self.screen == Screen::Leases
                ),
                action(
                    "project-tab-Console",
                    "AgentDocker commands",
                    Some(Message::Navigate(Screen::Console)),
                    self.screen == Screen::Console
                ),
                action(
                    "project-tab-Usage",
                    "Usage",
                    Some(Message::Navigate(Screen::Usage)),
                    self.screen == Screen::Usage
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
            content = content.push(card(more.wrap(), c));
        }
        // Under the tabs, where the chat it stands in for would be.
        if self.screen == Screen::Chat && self.shell.launch {
            content = content.push(self.launch_view(c));
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
            // The launch form takes the page; squeezed above the chat into
            // the header's 45% it hid its own Launch and Close.
            Screen::Chat if self.shell.launch => column![].into(),
            Screen::Chat => self.project_chat_view(c),
            Screen::Agents => self.sessions(c),
            Screen::Board => self.board_view(c),
            Screen::Usage => self.usage_view(c),
            Screen::Questions if self.has_conversations() => self.messages_view(c),
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
        // Chat owns the remaining viewport so its composer never depends on
        // scrolling past the header. Large forms and notices scroll above it.
        let workspace: Element<'_, Message> = if self.screen == Screen::Chat && !self.shell.launch {
            column![
                container(scrollable(content).height(iced::Shrink))
                    .max_height(self.shell.height / self.scale_factor() * 0.45),
                container(body).height(Fill),
            ]
            .spacing(18)
            .height(Fill)
            .into()
        } else {
            scrollable(content.push(body))
                .spacing(12)
                .width(Fill)
                .height(Fill)
                .id("workspace-scroll")
                .into()
        };
        row![
            container(Space::new().width(1).height(Fill)).style(move |_| c.rule()),
            container(workspace)
                .padding(if narrow { [18, 18] } else { [24, 30] })
                .width(Fill)
                .style(move |_| c.surface(c.ground, false))
        ]
        .height(Fill)
        .into()
    }

    /// One quiet line across the bottom: the daemon connection and the
    /// version. Said here once, so the rail and the pages need not repeat it.
    fn footer(&self, c: Colors) -> Element<'_, Message> {
        let connected = self.connected.is_ok();
        if let Some(notice) = self.daemon_notice(c) {
            return notice;
        }
        container(
            row![
                dot(if connected { c.green } else { c.amber }, 7.0, c),
                small(
                    if connected {
                        "Connected to the background service"
                    } else {
                        "Reconnecting to the background service…"
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

    /// One project's row in the sidebar, with its menu under it when open.
    fn project_row<'a>(
        &'a self,
        entry: &'a crate::catalog::Entry,
        shared: &std::collections::BTreeSet<String>,
        project_page: bool,
        c: Colors,
    ) -> Element<'a, Message> {
        let path = entry.project.root.clone();
        let selected = project_page && self.selected_root() == Some(path.as_path());
        let name = entry.name();
        let menu_open = self.shell.project_menu.as_deref() == Some(path.as_path());
        let live = self.live_in(&path);
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
        // One line each, clipped rather than wrapped or drawn over the
        // marks beside it. Two folders called the same are told apart
        // by where they are, in one short line.
        let mut label = column![
            text(name.clone())
                .size(14)
                .wrapping(iced::widget::text::Wrapping::None)
        ]
        .spacing(1)
        .width(Fill);
        if shared.contains(&name) {
            label = label.push(
                text(parent_folder(&path))
                    .size(12)
                    .color(c.muted)
                    .wrapping(iced::widget::text::Wrapping::None),
            );
        }
        let label = container(label).width(Fill).clip(true);
        let mut content = row![hint(monogram(&name, &seed, 20.0, c), hint_text, c), label]
            .spacing(6)
            .width(Fill)
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
        // The row's own menu, at the row's right edge: a quiet button
        // would otherwise take half the row's width as if it were a
        // row itself.
        content = content.push(
            container(custom(
                format!("project-menu-{}", path.display()),
                format!("Options for {name}"),
                icon(
                    Icon::More,
                    if menu_open { c.accent_ink } else { c.muted },
                    12.0,
                ),
                Some(Message::ProjectMenu(path.clone())),
                menu_open,
                Kind::Quiet,
                [4, 4],
            ))
            .width(26.0)
            .align_x(iced::alignment::Horizontal::Right),
        );
        let mut rows = column![].spacing(2).width(Fill);
        rows = rows.push(custom(
            format!("project-{}", path.display()),
            format!("{}{}", if entry.pinned { "• " } else { "" }, name),
            content,
            Some(Message::SelectProject(path.clone())),
            selected,
            Kind::Quiet,
            [8, 12],
        ));
        if menu_open {
            rows = rows.push(self.project_menu(entry, c));
        }
        rows.into()
    }

    /// Whether the Temporary fold is open: the person's choice once made,
    /// otherwise open while a scratch project has a live session or is the
    /// project on view. One rule for the fold as drawn and for what a
    /// click on it negates, so one click always closes an open fold.
    pub(super) fn temporary_fold_open(&self) -> bool {
        self.shell.temporary_open.unwrap_or_else(|| {
            let on_view = self.in_project();
            self.shell
                .catalog
                .projects
                .iter()
                .filter(|e| !e.pinned && crate::catalog::is_scratch(&e.project.root))
                .any(|e| {
                    self.live_in(&e.project.root) > 0
                        || (on_view && self.selected_root() == Some(e.project.root.as_path()))
                })
        })
    }

    /// Live, non-human sessions in the project at `root`.
    fn live_in(&self, root: &std::path::Path) -> usize {
        self.agents
            .iter()
            .filter(|a| {
                a.status.is_live()
                    && a.spec.runtime != agentdocker_core::HUMAN_RUNTIME
                    && a.project.as_ref().is_some_and(|p| p.root == root)
            })
            .count()
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
            if self.has_conversations() {
                "Messages"
            } else {
                "Inbox"
            },
            Icon::Inbox,
            {
                // With conversations the badge is their unread count, the
                // one number the screen itself shows per conversation.
                let now = Utc::now();
                let waiting = if self.has_conversations() {
                    self.unread_total() as usize
                } else {
                    self.questions.iter().filter(|q| !q.expired(now)).count()
                        + self.direct_messages().len()
                };
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
        let shared = self.shell.catalog.shared_names();
        // Folders discovered under a scratch directory and never pinned
        // are fixtures and trials, not the person's projects: one group
        // under the real ones, folded while nothing runs in any of them
        // and none is selected, opened by hand otherwise; the live count
        // sits on the fold so nothing running is ever hidden.
        let (temporary, regular): (Vec<&crate::catalog::Entry>, Vec<&crate::catalog::Entry>) = self
            .shell
            .catalog
            .projects
            .iter()
            .partition(|e| !e.pinned && crate::catalog::is_scratch(&e.project.root));
        for entry in regular {
            projects = projects.push(self.project_row(entry, &shared, project_page, c));
        }
        if !temporary.is_empty() {
            let live = temporary
                .iter()
                .map(|e| self.live_in(&e.project.root))
                .sum::<usize>();
            // The person's own choice wins, both ways; until they have
            // made one, the fold opens by itself for a live or selected
            // scratch project — the one rule in `temporary_fold_open`.
            let open = self.temporary_fold_open();
            let label = format!("Temporary ({})", temporary.len());
            let mut fold = row![
                text(if open { "▾" } else { "▸" }).size(11).color(c.muted),
                eyebrow(label.clone(), c).width(Fill),
            ]
            .spacing(8)
            .align_y(Center);
            if live > 0 {
                fold = fold.push(
                    row![dot(c.green, 6.0, c), small(live.to_string(), c)]
                        .spacing(5)
                        .align_y(Center),
                );
            }
            projects = projects.push(custom(
                "projects-temporary",
                label,
                fold,
                Some(Message::ToggleTemporary),
                false,
                Kind::Quiet,
                [6, 12],
            ));
            if open {
                for entry in temporary {
                    projects = projects.push(self.project_row(entry, &shared, project_page, c));
                }
            }
        }
        if self.agents.iter().any(|a| {
            a.project
                .as_ref()
                .is_none_or(|p| self.shell.catalog.broad_unpinned(&p.root))
                && a.spec.runtime != agentdocker_core::HUMAN_RUNTIME
        }) || self.discovered.iter().any(|p| {
            p.project
                .as_ref()
                .is_none_or(|p| self.shell.catalog.broad_unpinned(&p.root))
        }) {
            projects = projects.push(block_button(
                "unassigned",
                "Other sessions",
                Some(Message::Unassigned),
                self.shell.catalog.unassigned && project_page,
            ));
        }
        nav = nav
            .push(
                // A thin bar: the default one took a name's last letters
                // whenever the list scrolled.
                scrollable(projects)
                    .id("sidebar-projects")
                    .direction(iced::widget::scrollable::Direction::Vertical(
                        iced::widget::scrollable::Scrollbar::new()
                            .width(4)
                            .scroller_width(4)
                            .spacing(2),
                    ))
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
            // Wide, the rail is a pane whose divider the person drags;
            // narrow, it keeps a fixed width beside the workspace.
            .width(if self.narrow() {
                iced::Length::Fixed(204.0)
            } else {
                Fill
            })
            .height(Fill)
            .style(move |_| c.surface(c.sidebar, false))
            .into()
    }

    /// The menu under a project row: rename, pin, remove. Removing keeps
    /// the folder off the list until it is added again; nothing on disk
    /// changes and no session stops.
    fn project_menu(&self, entry: &crate::catalog::Entry, c: Colors) -> Element<'_, Message> {
        let path = entry.project.root.clone();
        let key = path.display().to_string();
        let mut items = column![].spacing(4).width(Fill);
        if let Some((_, draft)) = self
            .shell
            .project_rename
            .as_ref()
            .filter(|(root, _)| root == &path)
        {
            items = items.push(
                row![
                    input(
                        format!("project-rename-{key}"),
                        "Name",
                        draft,
                        Message::ProjectRenameDraft,
                    ),
                    primary(
                        format!("project-rename-save-{key}"),
                        "Save",
                        Some(Message::ProjectRenameSubmit),
                    ),
                ]
                .spacing(6)
                .align_y(Center),
            );
            items = items.push(small("Empty goes back to the folder's name.", c));
        } else {
            items = items.push(
                row![
                    action(
                        format!("project-rename-start-{key}"),
                        "Rename…",
                        Some(Message::ProjectRenameStart(path.clone())),
                        false,
                    ),
                    action(
                        format!("project-pin-{key}"),
                        if entry.pinned { "Unpin" } else { "Pin" },
                        Some(Message::ProjectPin(path.clone())),
                        entry.pinned,
                    ),
                    action(
                        format!("project-remove-{key}"),
                        "Remove from list",
                        Some(Message::ProjectRemove(path.clone())),
                        false,
                    ),
                ]
                .spacing(6)
                .wrap(),
            );
        }
        items = items.push(small(shorten_home(&path), c));
        container(items)
            .padding([8, 10])
            .width(Fill)
            .style(move |_| c.surface(c.raised, true))
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
                    "{} {}: {}",
                    self.name_of(&question.from),
                    // An answer is kept for an ended session, but nobody
                    // is reading it now; say so before the person writes.
                    if self.agent_live(&question.from) {
                        "asks"
                    } else {
                        "asked before its session ended"
                    },
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
            .filter(|a| self.delivery_needs_you(a) && self.has_project(a.project.as_ref()))
        {
            let name = self.display_name(agent);
            let (line, control, label) =
                if let Some((_, state)) = agentdocker_core::provider_block(agent, &self.agents) {
                    (
                        format!(
                            "{name}: {}",
                            state.issue.as_ref().expect("blocked").kind.label()
                        ),
                        "provider",
                        "Details",
                    )
                } else if agent.status.is_live() {
                    (
                        format!("{name}: messages are not being delivered"),
                        "review",
                        "Review",
                    )
                } else {
                    (
                        format!(
                            "{name} ended {}",
                            undelivered_phrase(self.undelivered(agent))
                        ),
                        "review",
                        "Review",
                    )
                };
            // Review opens the session with its delivery review unfolded;
            // Details opens the session, whose header carries the block.
            // The review reads the session log, so while disconnected
            // Review opens the session and says it is the last known state.
            let open = if control == "review" && self.connected.is_ok() {
                Message::ReviewSession(agent.id.to_string())
            } else {
                Message::OpenSession(agent.id.to_string())
            };
            items.push((
                dot(c.amber, 8.0, c),
                line,
                action(
                    format!("needs-you-{control}-{}", agent.id),
                    label,
                    Some(open),
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
                    // The tool, not a process number: nothing else tells
                    // them apart until one is connected and named.
                    format!(
                        "{} is running here, not connected",
                        super::runtime_label(&process.runtime)
                    ),
                    action(
                        format!("needs-you-connect-{}", process.pid),
                        if self.shell.adopting.contains(&process.pid) {
                            "Connecting…"
                        } else {
                            "Connect"
                        },
                        (self.connected.is_ok() && !self.shell.adopting.contains(&process.pid))
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
                        format!("{} needs setup", runtime.label),
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

    /// The selected project's root as the daemon's selector, when one is.
    pub(super) fn selected_project_root(&self) -> Option<String> {
        self.shell
            .catalog
            .selected
            .as_ref()
            .map(|root| root.display().to_string())
    }

    /// The pause on the selected project, when it is paused.
    fn selected_pause(&self) -> Option<&agentdocker_core::Pause> {
        let entry = self.shell.catalog.selected()?;
        let id = entry.project.id();
        self.pauses.iter().find(|p| p.project == id)
    }

    /// Hold or release the project's agents: a quiet Pause… that opens a
    /// reason, and while paused the reason on the header with Resume.
    /// The form belongs to the project it was opened for; on another
    /// project the header shows that project's own state.
    pub(super) fn pause_controls(&self, c: Colors) -> Option<Element<'_, Message>> {
        let root = self.selected_project_root()?;
        let connected = self.connected.is_ok();
        let control = self.pause_states.get(&root);
        let sending = control.is_some_and(|control| control.pending.is_some());
        let mut controls = if let Some(pause) = self.selected_pause() {
            column![
                row![
                    text(format!("Paused · {}", first_line(&pause.reason, 48)))
                        .color(c.amber)
                        .width(Fill),
                    action(
                        "resume-project",
                        if sending { "Resuming…" } else { "Resume" },
                        (connected && !sending).then_some(Message::ResumeProject(root.clone())),
                        false
                    ),
                ]
                .spacing(8)
                .align_y(Center)
            ]
        } else if let Some(reason) = control.and_then(|control| control.draft.as_ref()) {
            let ready = connected && !sending && !reason.trim().is_empty();
            let draft_project = root.clone();
            column![
                input_submitting(
                    "pause-reason",
                    "Why: what the agents will read",
                    reason,
                    move |reason| Message::PauseDraft(draft_project.clone(), reason),
                    connected && !sending,
                    ready.then_some(Message::PauseSubmit(root.clone())),
                ),
                row![
                    primary(
                        "pause-submit",
                        if sending {
                            "Pausing…"
                        } else {
                            "Pause agents"
                        },
                        ready.then_some(Message::PauseSubmit(root.clone()))
                    ),
                    action(
                        "pause-cancel",
                        "Cancel",
                        (!sending).then_some(Message::PauseCancel(root.clone())),
                        false
                    ),
                ]
                .spacing(8)
                .align_y(Center),
            ]
        } else {
            column![action(
                "pause-project",
                "Pause…",
                (connected && !sending).then_some(Message::PauseStart(root)),
                false
            )]
        }
        .spacing(4);
        if let Some(error) = control.and_then(|control| control.error.as_ref()) {
            controls = controls.push(text(error.clone()).size(13).color(c.amber));
        }
        Some(controls.into())
    }

    /// The project's one primary action, when there is a project to act in.
    fn launch_button(&self) -> Option<Element<'_, Message>> {
        self.shell.catalog.selected()?;
        Some(primary(
            "launch-agent",
            "Launch agent…",
            (self.connected.is_ok() && self.shell.project_available != Some(false))
                .then_some(Message::OpenLaunch),
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
        let earlier = self.session_records(Filter::Earlier);
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
            Filter::Earlier => Vec::new(),
        };
        let count = records.len()
            + if filter == Filter::Current {
                available.len()
            } else {
                0
            };
        if !records.is_empty() {
            panel_col = panel_col.push(panel(self.session_rows(&records, c), c));
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
                            text(super::runtime_label(&process.runtime)).size(14),
                            small(
                                process
                                    .cwd
                                    .as_ref()
                                    .map(|cwd| shorten_home(cwd))
                                    .unwrap_or_else(|| "folder unknown".to_owned()),
                                c
                            )
                        ]
                        .spacing(2)
                        .width(Fill),
                        action(
                            format!("adopt-{}", process.pid),
                            if self.shell.adopting.contains(&process.pid) {
                                "Connecting…"
                            } else {
                                "Connect"
                            },
                            (self.connected.is_ok() && !self.shell.adopting.contains(&process.pid))
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
        // A search can match only the Earlier group, which opens below.
        // Those results must not be paired with a "No matching sessions" card.
        let earlier_matches = filter == Filter::Current
            && !earlier.is_empty()
            && !self.shell.search.trim().is_empty();
        if count == 0 && !earlier_matches {
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
                    Filter::Earlier => ("No sessions", "Nothing has run here yet."),
                }
            };
            panel_col = panel_col.push(empty(title_text, hint, None, c));
        }
        if filter == Filter::Current && !earlier.is_empty() {
            // Ended sessions are not a tab: one collapsed group under the
            // current ones, opened by a search that finds something there.
            let open = self.shell.earlier_open || !self.shell.search.trim().is_empty();
            let shown = self.shell.earlier_shown.max(EARLIER_PAGE);
            let mut group = column![custom(
                "sessions-earlier",
                format!("Earlier ({})", earlier.len()),
                row![
                    text(if open { "▾" } else { "▸" }).size(11).color(c.muted),
                    eyebrow(format!("Earlier ({})", earlier.len()), c),
                ]
                .spacing(8)
                .align_y(Center),
                Some(Message::ToggleEarlier),
                false,
                Kind::Quiet,
                [6, 12],
            )]
            .spacing(6);
            if open {
                // The newest first, a page at a time: an ended session
                // from last week is a click away, not on every screen.
                let page: Vec<&AgentRecord> = earlier.iter().copied().take(shown).collect();
                let older = earlier.len().saturating_sub(page.len());
                group = group.push(panel(self.session_rows(&page, c), c));
                if older > 0 {
                    group = group.push(
                        container(action(
                            "sessions-earlier-more",
                            format!("Show {} older", older.min(EARLIER_PAGE)),
                            Some(Message::MoreEarlier),
                            false,
                        ))
                        .padding([0, 12]),
                    );
                }
            }
            panel_col = panel_col.push(group);
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

    /// The rows of one list of sessions, with a project eyebrow before each
    /// project's rows when every project is on view.
    fn session_rows<'a>(&'a self, records: &[&'a AgentRecord], c: Colors) -> Element<'a, Message> {
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
                            .map(|p| {
                                self.shell
                                    .catalog
                                    .projects
                                    .iter()
                                    .find(|entry| entry.project.root == p.root)
                                    .map(|entry| entry.name())
                                    .unwrap_or_else(|| p.name())
                            })
                            .unwrap_or_else(|| "Other sessions".into()),
                        c,
                    ))
                    .padding([12, 12]),
                );
                previous_project = project_root;
            }
            let id = agent.id.to_string();
            let activity = self.activity_label(agent);
            let name = self.display_name(agent);
            // The branch is in the name already when the record has one;
            // the line under it says what the session is doing.
            let branch = agent
                .vcs
                .as_ref()
                .and_then(|v| v.branch.as_deref())
                .filter(|b| !name.contains(b));
            let meta = format!(
                "{}{}",
                branch.map(|b| format!("{b} · ")).unwrap_or_default(),
                activity
            );
            let spoken = format!("{name}\n{} · {}", agent.spec.runtime, meta);
            let mut lines = column![
                text(name.clone())
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
            // A generated name already reads as the tool ("Claude Code ·
            // 0180d761"); the runtime pill would say it twice. A chosen
            // name keeps the pill, which is then the only place the tool
            // is named.
            if !agent.name_is_generated() {
                content = content.push(pill(agent.spec.runtime.clone(), c.raised, c.muted, c));
            }
            // An ended Claude Code session that can come back says so on
            // its row: the one thing to do with it is right there, not
            // behind Details. While its resume is on its way the row says
            // so; a blocker (live, no conversation id, no tool) keeps the
            // row quiet, and Details says why.
            if agent.spec.runtime == "claude-code"
                && !agent.status.is_live()
                && self.reconnect_blocker(agent).is_none()
            {
                let reconnecting = self.shell.reconnecting.as_deref() == Some(id.as_str());
                content = content.push(
                    container(custom(
                        format!("row-reconnect-{id}"),
                        format!("Reconnect {name} here"),
                        text(if reconnecting {
                            "Reconnecting…"
                        } else {
                            "Reconnect here"
                        })
                        .size(13),
                        (!reconnecting && !self.shell.launching && self.connected.is_ok())
                            .then_some(Message::Reconnect(id.clone())),
                        false,
                        Kind::Secondary,
                        [5, 10],
                    ))
                    .align_x(iced::alignment::Horizontal::Right),
                );
            }
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
        rows.into()
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
                    heading(self.display_name(agent), 18),
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
            if paused && agent.status.is_live() {
                status.push("Not receiving messages".to_owned());
            } else if agent.status.is_live() {
                status.push(self.input_readiness(agent).to_owned());
            }
            if let Some(queue) = queue {
                status.push(queue);
            }
            if let Some(received_at) = delivery.and_then(|d| d.received_at) {
                status.push(format!(
                    "Last took a message {}",
                    ago(Utc::now(), received_at)
                ));
            }
            if !status.is_empty() {
                body = body.push(small(status.join(" · "), c));
            }
            if let Some((source, state)) = agentdocker_core::provider_block(agent, &self.agents) {
                let issue = state.issue.as_ref().expect("blocked");
                if let Some(reset) = issue.reset_at {
                    body = body.push(small(
                        format!("Provider reset: {} UTC", reset.format("%b %d %H:%M")),
                        c,
                    ));
                }
                body = body.push(small(
                    "Messages are kept. Resume after the provider is available.",
                    c,
                ));
                body = body.push(action(
                    "resume-provider",
                    "Resume delivery",
                    self.connected
                        .is_ok()
                        .then(|| Message::ResumeProvider(source.id.to_string(), state.observed_at)),
                    false,
                ));
            }
            // The daemon gave up starting the bound receiver: the one repair a
            // person can make from here, and the only control that makes one.
            if let Some(binding) = agent.input_binding.as_ref().filter(|b| b.restart.exhausted) {
                body = body.push(small(
                    format!(
                        "Message delivery could not be restarted after {} tries. Its messages are kept.",
                        binding.restart.attempts
                    ),
                    c,
                ));
                body = body.push(action(
                    "retry-receiver",
                    "Restart message delivery",
                    self.connected
                        .is_ok()
                        .then(|| Message::RetryController(agent.id.to_string())),
                    false,
                ));
            }
            // An ended session: its messages are kept in its queue. Say how
            // many, how to get them delivered, and let the person put the
            // notice away. Nothing waiting means nothing to review.
            if paused && !agent.status.is_live() {
                if self.ended_with_undelivered(agent) {
                    body = body.push(
                        text(format!(
                            "This session ended {}.",
                            undelivered_phrase(self.undelivered(agent))
                        ))
                        .size(13)
                        .color(c.amber),
                    );
                    body = body.push(note(
                        "They are kept. Resume the conversation from its project folder and they are delivered to it; nothing is sent anywhere else.",
                        c,
                    ));
                } else {
                    body = body.push(small(
                        "Session ended. Nothing is waiting to be delivered.",
                        c,
                    ));
                }
            }
            // Every notice an ended session raises can be put away: it can
            // report no recovery, so a block or a count would otherwise stay.
            if !agent.status.is_live()
                && self.delivery_needs_you(agent)
                && !self.notice_dismissed(agent)
            {
                body = body.push(action(
                    "dismiss-delivery",
                    "Dismiss",
                    Some(Message::DismissDelivery(id.clone())),
                    false,
                ));
            }
            if paused && agent.status.is_live() {
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
                if self.connected.is_err() {
                    body = body.push(small("Reconnect to the daemon to read the session log.", c));
                }
                if self.shell.review_delivery {
                    if let Some(reason) = delivery.and_then(|d| d.pause_reason.as_deref()) {
                        body = body.push(text(plain_pause_reason(reason)).size(13).color(c.amber));
                    }
                    body = body.push(note("Messages to this session are kept, not lost. Check the log below before restarting it or sending again.", c));
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
                            Ok(log) if log.is_empty() => {
                                body = body.push(small("The session log is empty.", c));
                            }
                            Ok(log) => {
                                body =
                                    body.push(scrollable(text(log.as_str()).size(12)).height(120));
                            }
                        }
                    } else {
                        body = body.push(small("Loading session log…", c));
                    }
                }
            }
        }
        // Answer opens the exact question, the way Needs you does; the
        // Messages screen alone would show no question selected.
        if let Some(question) = self
            .questions
            .iter()
            .find(|q| q.from == id && !q.expired(Utc::now()))
        {
            body = body.push(primary(
                "session-reply",
                "Answer",
                Some(Message::OpenQuestion(question.id.clone())),
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
            body = body.push(primary(
                "open-original-terminal",
                "Open original terminal",
                (!self.shell.terminal_opening).then(|| Message::OpenAgentTerminal(id.clone())),
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
                let target_for_notice = draft_key.clone();
                let target = draft_key.clone();
                let send = (!sending && !value.trim().is_empty() && self.connected.is_ok())
                    .then_some(Message::SendSession(draft_key));
                body = body
                    .push(composer(
                        "session-message-text",
                        target.clone(),
                        "Message this agent…",
                        value,
                        move |text| Message::SessionDraft(target.clone(), text),
                        true,
                        send.clone(),
                    ))
                    .push(primary(
                        "send-session-message",
                        if sending {
                            "Queueing…"
                        } else {
                            "Send message"
                        },
                        send,
                    ));
                if let Some(notice) = draft.and_then(|draft| {
                    super::send_readiness::notice(
                        draft,
                        super::shell::DeliveryTarget::Session(target_for_notice.clone()),
                        c,
                    )
                }) {
                    body = body.push(notice);
                }
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
        // A name of one's choosing, for a live session: the daemon keeps it
        // unique and every other view follows.
        if agent.status.is_live() && agent.spec.runtime != agentdocker_core::HUMAN_RUNTIME {
            match &self.shell.renaming {
                Some((renaming, draft)) if renaming == &id => {
                    let valid = agentdocker_core::agent::check_name(draft.trim());
                    body = body.push(
                        column![
                            crate::controls::input_submitting(
                                "rename-session",
                                "A name for this session",
                                draft,
                                Message::RenameDraft,
                                true,
                                (self.connected.is_ok() && valid.is_ok())
                                    .then_some(Message::SubmitRename),
                            ),
                            row![
                                primary(
                                    "rename-save",
                                    "Save name",
                                    (self.connected.is_ok() && valid.is_ok())
                                        .then_some(Message::SubmitRename),
                                ),
                                action(
                                    "rename-cancel",
                                    "Cancel",
                                    Some(Message::CancelRename),
                                    false
                                ),
                            ]
                            .spacing(6),
                        ]
                        .spacing(6),
                    );
                    if let Err(reason) = valid
                        && !draft.is_empty()
                    {
                        body = body.push(small(reason, c));
                    }
                }
                _ => {
                    body = body.push(action(
                        "rename-session",
                        "Rename…",
                        Some(Message::StartRename(id.clone())),
                        false,
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
            // The full id is 32 hex digits with nowhere to wrap: shown short,
            // copied whole.
            let mut details = column![
                kv("Session", agent.id.short().to_string(), c),
                action(
                    "copy-session-id",
                    "Copy session ID",
                    Some(Message::CopyGuidance(agent.id.to_string())),
                    false
                ),
            ]
            .spacing(6);
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
                details = details.push(kv("Terminal", session.describe(), c));
            }
            if let Some(guidance) =
                super::send_readiness::reconnect(agent, &self.agents, "inspector", c)
            {
                details = details.push(guidance);
                // The relaunch the guidance describes, done here: the
                // session's own tool, its conversation, its folder and the
                // channel, in a pane of this window where Claude's own
                // consent prompt appears. Until its terminal process has
                // ended the button says why it waits.
                if agent.spec.runtime == "claude-code" {
                    let blocker = self.reconnect_blocker(agent);
                    let mut reconnect = row![primary(
                        format!("reconnect-{}", agent.id),
                        if self.shell.reconnecting.as_deref() == Some(agent.id.as_str()) {
                            "Reconnecting…"
                        } else {
                            "Reconnect here"
                        },
                        (blocker.is_none() && !self.shell.launching && self.connected.is_ok())
                            .then_some(Message::Reconnect(agent.id.to_string())),
                    )]
                    .spacing(8)
                    .align_y(Center);
                    if let Some(why) = blocker {
                        reconnect = reconnect.push(small(why, c));
                    } else {
                        reconnect = reconnect.push(small(
                            "Opens the session in a pane here, with its conversation and live messages; accept Claude's prompt there.",
                            c,
                        ));
                    }
                    details = details.push(reconnect);
                }
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
                "Name (optional; one is made up otherwise)",
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
            // The normal launch has a receiver. Turning it off is explicit;
            // provider consent remains a separate provider-owned step.
            tools = tools.push(
                column![
                    action(
                        "launch-idle-input",
                        if self.shell.launch_channel { "Idle messages: On" } else { "Idle messages: Off" },
                        (!self.shell.launching).then_some(Message::LaunchChannel(!self.shell.launch_channel)),
                        self.shell.launch_channel,
                    ),
                    small(
                        if !self.shell.launch_channel {
                            "Messages may wait until you interact with this session."
                        } else if self.shell.launch_runtime.as_deref() == Some("codex") {
                            "Opens a Codex conversation here. Messages wait until the current turn finishes."
                        } else { "Connects Claude's experimental channel. Complete Claude's consent in the terminal." },
                        c
                    )
                ]
                .spacing(4),
            );
        }
        if self.shell.launch_runtime.is_some()
            && !matches!(
                self.shell.launch_runtime.as_deref(),
                Some("claude-code" | "codex")
            )
        {
            tools = tools.push(small(
                "Automatic idle delivery is not available for this tool.",
                c,
            ));
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
            let agent = self.agents.iter().find(|a| a.id.as_str() == id);
            let live = agent.is_some_and(|a| a.status.is_live());
            let has_project = agent.is_some_and(|a| a.project.is_some());
            let entry = self.shell.session_drafts.get(id);
            let draft = entry.map(|e| e.draft.text.clone()).unwrap_or_default();
            let sending = entry.is_some_and(|e| e.draft.sending.is_some());
            let ready = live && self.connected.is_ok() && !sending && !draft.trim().is_empty();
            let owner = id.to_owned();
            let mut composer = column![
                row![
                    composer(
                        format!("reply-{id}"),
                        id.to_owned(),
                        "Message…",
                        &draft,
                        move |t| Message::SessionDraft(owner.clone(), t),
                        live && !sending,
                        ready.then_some(Message::SendSession(id.to_owned())),
                    ),
                    primary(
                        format!("send-reply-{id}"),
                        if sending { "Sending…" } else { "Send" },
                        ready.then_some(Message::SendSession(id.to_owned())),
                    ),
                    action(
                        format!("send-everyone-{id}"),
                        "Send to everyone",
                        (ready && has_project).then_some(Message::SendProject(id.to_owned())),
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
                composer = composer.push(small("Queued for the agent.", c));
            } else {
                composer = composer.push(small(
                    "Send goes to this agent. Send to everyone reaches every agent in its project.",
                    c,
                ));
            }
            if let Some(notice) = entry.and_then(|entry| {
                super::send_readiness::notice(
                    &entry.draft,
                    super::shell::DeliveryTarget::Session(id.to_owned()),
                    c,
                )
            }) {
                composer = composer.push(notice);
            }
            convo = convo.push(composer);
        } else {
            convo = convo.push(small("Choose a conversation to reply.", c));
        }

        if self.narrow() {
            // One column: the list, or the conversation with a way back.
            // Choosing a conversation must show it, not append it below a
            // list the reader then has to scroll past.
            if self.shell.inbox_open {
                let back = custom(
                    "thread-back",
                    "Conversations",
                    row![text("‹ Conversations").size(14)],
                    Some(Message::InboxList),
                    false,
                    Kind::Quiet,
                    [8, 10],
                );
                column![back, convo].spacing(14).into()
            } else {
                column![panel(list, c)].spacing(14).into()
            }
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
        let question = message.kind == "question";
        let long = question || payload.chars().count() > FOLD || payload.lines().count() > 8;
        let expanded = self.shell.message_detail.as_ref() == Some(&id);
        let shown_text = if question && !expanded {
            first_line(&payload, 160)
        } else if long && !expanded {
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
                match (question, expanded) {
                    (true, true) => "Hide question",
                    (true, false) => "Show question",
                    (false, true) => "Show less",
                    (false, false) => "Show more",
                },
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
    pub(super) fn question_card(&self, question: &Question, c: Colors) -> Element<'_, Message> {
        {
            let id = question.id.clone();
            let draft_id = id.clone();
            let busy = self.sending.contains(&id);
            let expired = question.expired(Utc::now());
            let answer = self.shell.answers.get(&id).cloned().unwrap_or_default();
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
                    let send = (enabled && !answer.trim().is_empty())
                        .then_some(Message::Answer(id.clone()));
                    body = body
                        .push(composer(
                            format!("answer-{id}"),
                            id.to_string(),
                            if presentation.is_some() {
                                "Or write an answer"
                            } else {
                                "Your answer"
                            },
                            &answer,
                            move |text| Message::Draft(draft_id.clone(), text),
                            enabled,
                            send.clone(),
                        ))
                        .push(primary(
                            format!("send-answer-{id}"),
                            if busy { "Sending…" } else { "Send answer" },
                            send,
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
        // With no project chosen (All projects) every channel is shown;
        // "Reviews" from a conversation lands here from anywhere and must not
        // find "No channels yet" about the one just open. Channels agents
        // opened come first; the rooms AgentDocker opens when checkouts
        // overlap sit folded behind Overlaps (n), as Messages folds them.
        let on_view: Vec<_> = self
            .channels
            .iter()
            .filter(|ch| {
                selected
                    .as_ref()
                    .is_none_or(|project| &ch.project == project)
            })
            .collect();
        let is_overlap = |ch: &&agentdocker_core::Channel| {
            matches!(
                ch.subject,
                agentdocker_core::ChannelSubject::Contested { .. }
            )
        };
        let overlaps = on_view.iter().filter(|ch| is_overlap(ch)).count();
        let named: Vec<_> = on_view
            .iter()
            .filter(|ch| !is_overlap(ch))
            .copied()
            .collect();
        let folded: Vec<_> = on_view
            .iter()
            .filter(|ch| is_overlap(ch))
            .copied()
            .collect();
        let count = on_view.len();
        // Open, the fold sits where the overlap rooms begin.
        let fold_at = (overlaps > 0 && self.shell.overlaps_open).then_some(named.len());
        let mut shown = named;
        if self.shell.overlaps_open {
            shown.extend(folded);
        }
        for (index, channel) in shown.into_iter().enumerate() {
            if fold_at == Some(index) {
                list = list.push(self.overlaps_toggle(overlaps, c));
            }
            let id = channel.id.to_string();
            let open = channel.is_open();
            let (title, detail) = channel_heading(channel);
            let mut body = column![
                row![
                    heading(title, 17).width(Fill),
                    pill(
                        if open { "Open" } else { "Closed" },
                        if open { alpha(c.green, 0.16) } else { c.raised },
                        if open { c.green } else { c.muted },
                        c
                    )
                ]
                .spacing(10)
                .align_y(Center),
                small(self.members_line(&channel.members), c)
            ]
            .spacing(6);
            if let Some(detail) = detail {
                body = body.push(small(detail, c));
            }
            for review in &channel.reviews {
                body = body.push(
                    text(format!(
                        "{} · {} on {}'s work: {}",
                        match review.verdict {
                            agentdocker_core::channel::Verdict::Approve => "Approved",
                            agentdocker_core::channel::Verdict::Changes => "Changes asked for",
                            agentdocker_core::channel::Verdict::Comment => "Comment",
                        },
                        review.by_name,
                        review.of_name,
                        review.note
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
                // A closed channel takes no messages: the control would do
                // nothing, so it is not offered as if it would.
                (self.connected.is_ok() && open).then_some(Message::ChannelTarget(id.clone())),
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
                if let Some(notice) = super::send_readiness::notice(
                    &draft,
                    super::shell::DeliveryTarget::Channel(id.clone()),
                    c,
                ) {
                    body = body.push(notice);
                }
                body = body.push(
                    row![
                        composer(
                            "channel-message",
                            id.clone(),
                            "Message",
                            &draft.text,
                            Message::ChannelDraft,
                            draft.sending.is_none(),
                            (draft.sending.is_none()
                                && self.connected.is_ok()
                                && !draft.text.trim().is_empty())
                            .then_some(Message::SendChannel),
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
        // Folded, the overlap rooms are not in the loop: the fold that
        // opens them follows the named channels.
        if overlaps > 0 && !self.shell.overlaps_open {
            list = list.push(self.overlaps_toggle(overlaps, c));
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

    /// The fold for AgentDocker's overlap rooms on the Channels screen.
    fn overlaps_toggle(&self, count: usize, c: Colors) -> Element<'_, Message> {
        row![
            action(
                "channels-overlaps",
                format!(
                    "{} Overlaps ({count})",
                    if self.shell.overlaps_open {
                        "▾"
                    } else {
                        "▸"
                    }
                ),
                Some(Message::ToggleOverlaps),
                false,
            ),
            small(
                "Rooms AgentDocker opens when two checkouts change the same files",
                c
            ),
        ]
        .spacing(10)
        .align_y(Center)
        .into()
    }

    /// Who is in a channel, in one line: the count and at most four names.
    fn members_line(&self, members: &[agentdocker_core::AgentId]) -> String {
        const NAMED: usize = 4;
        let names: Vec<String> = members
            .iter()
            .take(NAMED)
            .map(|id| self.name_of(id.as_str()))
            .collect();
        let rest = members.len().saturating_sub(NAMED);
        let who = if rest > 0 {
            format!("{} and {rest} more", names.join(", "))
        } else {
            names.join(", ")
        };
        format!(
            "{} member{} · {who}",
            members.len(),
            if members.len() == 1 { "" } else { "s" }
        )
    }

    fn journal_view(&self, c: Colors) -> Element<'_, Message> {
        let list = column![].spacing(12);
        if self.journal.is_empty() {
            return list
                .push(empty(
                    "No activity recorded yet",
                    "Commits, arrivals, finished work and notes from this project appear here.",
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
                            text(self.journal_line(entry)).size(14),
                            small(ago(Utc::now(), entry.at), c)
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
            heading("Files in use", 18),
            note(
                "What each agent has said it is working on. Others wait, or ask, before touching the same thing.",
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
                    heading(resource_label(&lease.resource.to_string()), 15).width(Fill),
                    pill(
                        match lease.mode {
                            agentdocker_core::LeaseMode::Exclusive => "Only this agent",
                            agentdocker_core::LeaseMode::Shared => "Shared",
                        },
                        c.accent_soft,
                        c.accent_ink,
                        c
                    )
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
                "Nothing in use",
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
                    "A command, e.g. ps, leases or journal",
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
                    (!self.console_history.is_empty()).then_some(Message::Recall(true)),
                    false
                ),
                action(
                    "next-command",
                    "Next",
                    (!self.console_history.is_empty()).then_some(Message::Recall(false)),
                    false
                )
            ]
            .spacing(6),
            // No empty black box before anything has run.
            if self.console_output.is_empty() {
                Element::from(note("Output appears here.", c))
            } else {
                container(
                    text(self.console_output.clone())
                        .font(Font::MONOSPACE)
                        .size(self.settings.terminal_size)
                        .color(ink),
                )
                .padding(14)
                .width(Fill)
                .style(move |_| c.surface(ground, true))
                .into()
            }
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
            let supported = runtime.mcp != agentdocker_core::runtime::Wiring::Unsupported
                || runtime.hooks != agentdocker_core::runtime::Wiring::Unsupported;
            let unverified = runtime.mcp == agentdocker_core::runtime::Wiring::Unverified
                || runtime.hooks == agentdocker_core::runtime::Wiring::Unverified;
            let sessions: Vec<_> = self
                .agents
                .iter()
                .filter(|agent| agent.spec.runtime == runtime.name && agent.status.is_live())
                .collect();
            let ready = self.connected.is_ok()
                && sessions.iter().any(|agent| {
                    agent.input_delivery.as_ref().is_some_and(|delivery| {
                        delivery.current_for(agent.process_started_at, Utc::now())
                    })
                });
            let missing = installed
                && (runtime.mcp == agentdocker_core::runtime::Wiring::Missing
                    || runtime.hooks == agentdocker_core::runtime::Wiring::Missing);
            // A session with a receiver that is paused, or whose report has
            // gone stale, is not one whose input waits for a prompt: it is
            // one whose route needs attention, and it says which.
            let paused = sessions.iter().any(|agent| {
                agent
                    .input_delivery
                    .as_ref()
                    .is_some_and(|delivery| delivery.paused_for(agent.process_started_at))
            });
            let stale = !paused
                && sessions.iter().any(|agent| {
                    agent.input_delivery.as_ref().is_some_and(|delivery| {
                        agent.process_started_at == Some(delivery.process_started_at)
                            && !delivery.current_for(agent.process_started_at, Utc::now())
                    })
                });
            // Keep the overview compact; individual sessions and receipts
            // remain in Details. Saved configuration is not contact evidence.
            // "Needs setup" says what: a release can require a hook event
            // a wired machine never had, and the person has to be told
            // which, not sent to look. "Connected" says what is not
            // there either: with no input route at all, nothing reaches
            // the session while it is idle, so messages wait for its next
            // prompt; with a route that is paused or silent, they wait for
            // that to be put right.
            let (mark, word) = if ready {
                (c.green, "Receiving messages".to_owned())
            } else if reporting && paused {
                (c.amber, "Connected · messages paused".to_owned())
            } else if reporting && stale {
                (c.amber, "Connected · not heard from recently".to_owned())
            } else if reporting {
                (
                    c.cyan,
                    "Connected · messages wait for its next prompt".to_owned(),
                )
            } else if !installed {
                (c.faint, "Not installed".to_owned())
            } else if runtime.in_browser() {
                // No session of it appears unless one came in through the
                // connector: say which here, where the person comes to ask
                // why their browser agent is or is not listed.
                (
                    if sessions.is_empty() {
                        c.faint
                    } else {
                        c.green
                    },
                    super::in_browser_word(runtime, sessions.len(), self.connector.as_ref()),
                )
            } else if !supported {
                (c.faint, "Installed · integration unavailable".to_owned())
            } else if unverified {
                (c.amber, "Setup needs review".to_owned())
            } else if missing {
                (c.amber, super::missing_setup(runtime))
            } else {
                (c.cyan, "Configured · waiting for contact".to_owned())
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
            if installed && supported && (missing || unverified) && !reporting {
                actions = actions.push(primary(
                    format!("setup-{}", runtime.name),
                    "Set up",
                    (!self.setup_busy).then_some(Message::Setup(vec![
                        runtime.name.clone(),
                        "--preview".into(),
                    ])),
                ));
            }
            // A `claude` typed in a terminal only sees messages at its next
            // prompt unless the shell adds the channel flag; one reviewed
            // change does that, and this is where the person looks for it.
            let terminal_wake = runtime.name == "claude-code"
                && installed
                && matches!(
                    runtime.shell,
                    agentdocker_core::runtime::Wiring::Missing
                        | agentdocker_core::runtime::Wiring::Unverified
                );
            if terminal_wake {
                actions = actions.push(action(
                    "setup-shell",
                    "Wake terminal sessions",
                    (!self.setup_busy)
                        .then_some(Message::Setup(vec!["--shell".into(), "--preview".into()])),
                    false,
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
                if runtime.name == "claude-code" {
                    facts = facts.push(kv(
                        "Terminal launches",
                        match runtime.shell {
                            agentdocker_core::runtime::Wiring::Wired => {
                                "wake on messages (shell startup file carries the channel flag)"
                            }
                            agentdocker_core::runtime::Wiring::Missing => {
                                "see messages at their next prompt; Wake terminal sessions adds the channel flag to every `claude`"
                            }
                            agentdocker_core::runtime::Wiring::Unverified => {
                                "an older agentdocker block is in the shell startup file; Wake terminal sessions replaces it"
                            }
                            agentdocker_core::runtime::Wiring::Unsupported => {
                                "your shell is not one setup knows (zsh, bash, fish); start claude with --dangerously-load-development-channels server:agentdocker yourself"
                            }
                        },
                        c,
                    ));
                }
                for app in &runtime.apps {
                    facts = facts.push(kv("Application", app.label.clone(), c));
                }
                for extension in &runtime.extensions {
                    facts = facts.push(kv("Browser", super::extension_words(extension), c));
                    if let Some(bridge) = &extension.bridge {
                        facts = facts.push(kv("Bridge", bridge.display().to_string(), c));
                    }
                }
                if runtime.in_browser() && installed {
                    facts = facts.push(note(agentdocker_core::runtime::IN_BROWSER, c));
                    // The connector is what brings a browser agent here:
                    // its address and pairing code are what the person
                    // needs at the vendor's settings and on the consent
                    // page, and this card is where they look for them.
                    match &self.connector {
                        Some(serving) => {
                            facts = facts.push(kv("Connector", serving.mcp_url(), c));
                            facts = facts.push(kv(
                                "Pairing code",
                                format!(
                                    "{} · typed on the consent page, which also asks which project the agent joins",
                                    serving.pairing_code
                                ),
                                c,
                            ));
                            facts = facts.push(kv(
                                "Add it",
                                super::add_connector_words(&runtime.name),
                                c,
                            ));
                        }
                        None => {
                            facts = facts.push(kv(
                                "Connector",
                                "not running · `agentdocker connector install --tunnel tailscale` (or `--tunnel cloudflared`) serves one for every project on this machine",
                                c,
                            ));
                        }
                    }
                }
                for why in &runtime.incomplete {
                    facts = facts.push(note(format!("Inventory incomplete: {why}"), c));
                }
                if installed && supported && !missing && !unverified && !reporting {
                    facts = facts.push(note(
                        "Setup is saved. Start a fresh session to load it. Approve only the \
                         integration prompts shown by the provider.",
                        c,
                    ));
                }
                for agent in &sessions {
                    let now = Utc::now();
                    let seen = |kind| {
                        agent.adapter_contacts.get(&kind).is_some_and(|contact| {
                            self.connected.is_ok()
                                && contact.current_for(agent.process_started_at, now)
                        })
                    };
                    facts = facts
                        .push(rule(c))
                        .push(heading(self.display_name(agent), 14))
                        .push(small(self.input_readiness(agent), c))
                        .push(small(
                            format!(
                                "MCP: {} · Hooks: {}",
                                if seen(agentdocker_core::AdapterKind::Mcp) {
                                    "recent contact"
                                } else {
                                    "no recent contact"
                                },
                                if seen(agentdocker_core::AdapterKind::Hooks) {
                                    "recent contact"
                                } else {
                                    "no recent contact"
                                }
                            ),
                            c,
                        ));
                    if let Some(guidance) =
                        super::send_readiness::reconnect(agent, &self.agents, "tools", c)
                    {
                        facts = facts.push(guidance);
                    }
                }
                if !sessions.is_empty()
                    && !ready
                    && matches!(runtime.name.as_str(), "codex" | "claude-code")
                    && sessions.iter().any(|agent| {
                        agent.input_delivery.as_ref().is_none_or(|delivery| {
                            Some(delivery.process_started_at) != agent.process_started_at
                        })
                    })
                {
                    facts = facts.push(note(
                        if runtime.name == "claude-code" {
                            "Hooks cannot start an idle turn. New Claude launches use Idle messages: On and require channel consent. Existing sessions need a safe reconnect; queued messages stay with their current record."
                        } else {
                            "Hooks cannot start an idle turn. Codex sessions need their message delivery connected. New launches here use Idle messages: On."
                        },
                        c,
                    ));
                }
                facts = facts.push(
                    row![
                        action(
                            format!("setup-review-{}", runtime.name),
                            "Review setup",
                            (!self.setup_busy && supported).then_some(Message::Setup(vec![
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
                    small(
                        if ok {
                            "Configuration checked"
                        } else {
                            "Needs attention"
                        },
                        c
                    )
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
        let shell_plan = changes
            .into_iter()
            .flatten()
            .any(|change| value(change, "channel") == "shell");
        let title_text = match (&tool, phase.as_str()) {
            (Some(_), "applied") if shell_plan => "Terminal launches will wake".to_owned(),
            (Some(_), "undone") if shell_plan => "Shell change undone".to_owned(),
            (Some(_), _) if shell_plan => "Wake terminal sessions".to_owned(),
            (Some(tool), "applied") => format!("{tool} setup saved"),
            (Some(tool), "undone") => format!("{tool} setup undone"),
            (Some(tool), _) => format!("Connect {tool}"),
            (None, "applied") => "Setup saved".to_owned(),
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
                "shell" => "Terminal launches wake (shell startup file)".to_owned(),
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
                    "Preview builds allowed"
                } else {
                    "Allow preview builds"
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
            let mut facts = if update["published"] == false {
                // Nothing on either channel yet: an answer, not a failure.
                column![
                    heading("No update published yet", 16),
                    kv("Installed", installed, c),
                    note(
                        "Nothing newer has been published for this installation. Check again later.",
                        c
                    ),
                ]
                .spacing(6)
            } else {
                column![
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
                .spacing(6)
            };
            if p.preview_consent_needed() {
                facts = facts.push(note(
                    format!(
                        "{available} is a preview build. Allow preview builds above to download it."
                    ),
                    c,
                ));
            }
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

/// A channel's heading and, for an overlap room, the paths it is about in
/// one bounded line: "Contested paths (338)" over "view.rs, shell.rs and
/// 336 more", never the list itself as a title.
fn channel_heading(channel: &agentdocker_core::Channel) -> (String, Option<String>) {
    match &channel.subject {
        agentdocker_core::ChannelSubject::Contested { paths } if !paths.is_empty() => {
            const NAMED: usize = 3;
            let named: Vec<String> = paths
                .iter()
                .take(NAMED)
                .map(|p| p.display().to_string())
                .collect();
            let rest = paths.len().saturating_sub(NAMED);
            let detail = if rest > 0 {
                format!("{} and {rest} more", named.join(", "))
            } else {
                named.join(", ")
            };
            (format!("Contested paths ({})", paths.len()), Some(detail))
        }
        _ => (channel.title(), None),
    }
}

/// A held resource as a person reads it: a path as a path (home as `~`),
/// anything else as `kind: value`.
fn resource_label(key: &str) -> String {
    match key.split_once(':') {
        Some(("path", path)) => shorten_home(std::path::Path::new(path)),
        Some((kind, value)) => format!("{kind}: {value}"),
        None => key.to_owned(),
    }
}

/// How an ended session's undelivered messages read after "ended".
fn undelivered_phrase(count: Option<usize>) -> String {
    match count {
        Some(1) => "with 1 message not delivered".to_owned(),
        Some(n) => format!("with {n} messages not delivered"),
        None => "before its messages were delivered".to_owned(),
    }
}

/// The daemon's reason for pausing delivery, in the person's words. The
/// daemon's own text stays in the session log and the CLI; this is what the
/// app says about it. Unknown reasons are shown as they are.
pub(super) fn plain_pause_reason(reason: &str) -> String {
    use agentdocker_core::input::{
        PAUSE_CONTROLLER_ENDED, PAUSE_CONTROLLER_RESTART_FAILED, PAUSE_RECEIVER_UPGRADING,
    };
    match reason {
        PAUSE_CONTROLLER_ENDED => {
            "The helper that hands messages to this session stopped.".to_owned()
        }
        PAUSE_CONTROLLER_RESTART_FAILED => {
            "The helper that hands messages to this session could not be started again.".to_owned()
        }
        PAUSE_RECEIVER_UPGRADING => {
            "Message delivery is being updated; it resumes on its own.".to_owned()
        }
        other => other.to_owned(),
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

/// Where a folder is, short enough for one line: the home folder as `~`,
/// and a deep path kept to its last two folders, to tell two folders of
/// one name apart.
fn parent_folder(path: &std::path::Path) -> String {
    let Some(parent) = path.parent() else {
        return path.display().to_string();
    };
    // The part that tells two folders of one name apart comes first, so a
    // clipped line still carries it: `agentdocker-delivery · /private/tmp`,
    // not `/private/tmp/agentd…`.
    let Some(name) = parent.file_name() else {
        return shorten_home(parent);
    };
    match parent.parent() {
        Some(above) if above.parent().is_some() => {
            let shown = shorten_home(above);
            let parts: Vec<&str> = shown.split('/').filter(|p| !p.is_empty()).collect();
            let above = if parts.len() <= 3 {
                shown
            } else {
                format!("…/{}", parts[parts.len() - 1])
            };
            format!("{} · {above}", name.to_string_lossy())
        }
        _ => name.to_string_lossy().into_owned(),
    }
}

/// A path with the home folder as `~`.
fn shorten_home(path: &std::path::Path) -> String {
    let shown = path.display().to_string();
    match std::env::var_os("HOME").map(std::path::PathBuf::from) {
        Some(home) if path.starts_with(&home) => match path.strip_prefix(&home) {
            Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => shown,
        },
        _ => shown,
    }
}

/// How a divider between panes looks: nothing until pointed at, then a
/// hairline in the accent, and the accent while it is being dragged.
pub(super) fn split_style(c: Colors) -> iced::widget::pane_grid::Style {
    use iced::widget::pane_grid::{Highlight, Line, Style};
    Style {
        hovered_region: Highlight {
            background: iced::Background::Color(iced::Color::TRANSPARENT),
            border: iced::Border::default(),
        },
        picked_split: Line {
            color: c.accent,
            width: 2.0,
        },
        hovered_split: Line {
            color: c.accent,
            width: 2.0,
        },
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_shared_folder_name_is_told_apart_by_its_parent_first() {
        assert_eq!(
            super::parent_folder(std::path::Path::new(
                "/private/tmp/agentdocker-delivery/workspace"
            )),
            "agentdocker-delivery · /private/tmp"
        );
        assert_eq!(
            super::parent_folder(std::path::Path::new("/srv/workspace")),
            "srv"
        );
    }

    #[test]
    fn daemon_pause_reasons_read_in_the_persons_words() {
        use agentdocker_core::input::{
            PAUSE_CONTROLLER_ENDED, PAUSE_CONTROLLER_RESTART_FAILED, PAUSE_RECEIVER_UPGRADING,
        };
        for reason in [
            PAUSE_CONTROLLER_ENDED,
            PAUSE_CONTROLLER_RESTART_FAILED,
            PAUSE_RECEIVER_UPGRADING,
        ] {
            let plain = super::plain_pause_reason(reason);
            assert_ne!(plain, reason);
            assert!(!plain.contains("controller") && !plain.contains("receiver"));
        }
        assert_eq!(super::plain_pause_reason("something new"), "something new");
        assert_eq!(
            super::undelivered_phrase(Some(1)),
            "with 1 message not delivered"
        );
        assert_eq!(
            super::undelivered_phrase(Some(3)),
            "with 3 messages not delivered"
        );
    }

    use super::{remaining_fraction, spoken_payload};
    use chrono::{Duration, Utc};
    use serde_json::json;

    #[test]
    fn tools_require_session_contact_and_never_infer_input_from_general_activity() {
        use agentdocker_core::{
            AdapterContact, AdapterKind, AgentRecord, AgentSpec, AgentStatus, InputDelivery,
        };
        let (tx, _commands) = crate::app::queue::channel();
        let (_sender, rx) = std::sync::mpsc::channel();
        let mut app = crate::app::App::bare(tx, rx);
        let now = Utc::now();
        let birth = now - Duration::seconds(10);
        let mut agent = AgentRecord::new(
            AgentSpec {
                runtime: "codex".into(),
                ..Default::default()
            },
            false,
            birth,
        );
        agent.status = AgentStatus::Running;
        agent.process_started_at = Some(birth);
        app.activity.insert(
            agent.id.to_string(),
            agentdocker_core::Activity::Working { since: now },
        );
        app.agents.push(agent.clone());
        assert!(
            !app.tool_reports("codex"),
            "leases and coordination are not adapter contact"
        );
        assert_eq!(
            app.input_readiness(&agent),
            "Messages may wait for its next prompt"
        );
        agent.adapter_contacts.insert(
            AdapterKind::Mcp,
            AdapterContact {
                process_started_at: birth,
                observed_at: now,
            },
        );
        app.agents[0] = agent.clone();
        assert!(app.tool_reports("codex"));
        assert!(
            !app.tool_reports("claude-code"),
            "contact is specific to the runtime"
        );
        assert_eq!(
            app.input_readiness(&agent),
            "Messages may wait for its next prompt",
            "MCP contact does not prove idle wake"
        );
        agent.input_delivery = Some(InputDelivery {
            process_started_at: birth,
            paused: false,
            pause_reason: None,
            reported_at: now,
            received: None,
            received_at: None,
        });
        assert_eq!(app.input_readiness(&agent), "Ready for messages");
        // Words queued behind a current receiver that no receipt covers are
        // waiting on the provider; an earlier receipt does not make them
        // delivered, and a receipt for them does, acknowledged or not.
        app.queued_inputs.insert(agent.id.to_string(), 2);
        app.awaiting_receipt.insert(agent.id.to_string(), 2);
        assert_eq!(
            app.input_readiness(&agent),
            "Sent · waiting for the agent to take it"
        );
        agent.input_delivery.as_mut().unwrap().received = Some(agentdocker_core::ReceivedInput {
            messages: vec!["m1".to_owned().into()],
            receipt: agentdocker_core::InputReceipt::ClaudeChannel,
        });
        agent.input_delivery.as_mut().unwrap().received_at = Some(now);
        app.awaiting_receipt.insert(agent.id.to_string(), 1);
        assert_eq!(
            app.input_readiness(&agent),
            "Sent · waiting for the agent to take it"
        );
        app.awaiting_receipt.insert(agent.id.to_string(), 0);
        assert_eq!(app.input_readiness(&agent), "Receiving messages");
        agent.input_delivery.as_mut().unwrap().received = None;
        agent.input_delivery.as_mut().unwrap().received_at = None;
        app.queued_inputs.remove(agent.id.as_str());
        app.awaiting_receipt.remove(agent.id.as_str());
        agent.process_started_at = Some(now);
        app.agents[0] = agent.clone();
        assert!(
            !app.tool_reports("codex"),
            "a new process cannot inherit contact"
        );
        assert_eq!(app.input_readiness(&agent), "Not heard from recently");
        agent.process_started_at = Some(birth);
        agent.input_delivery.as_mut().unwrap().paused = true;
        assert_eq!(app.input_readiness(&agent), "Not receiving messages");
        app.connected = Err("offline".into());
        assert!(!app.tool_reports("codex"));
        assert_eq!(app.input_readiness(&agent), "Readiness unavailable");
    }

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
