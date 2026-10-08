//! Tools, the setup review, Settings, the agent terminal's frame, the
//! command console and Installation: the screens where the person wires
//! their tools to AgentDocker and looks after the app itself.
//!
//! They share one grammar. A card holds rows separated by hairlines; a
//! row is a title over one quiet line on the left and a single control on
//! the right. Status is a dot and a word. A screen has at most one filled
//! action, and a row only gets one when it is the row that needs doing.
use super::icons::{Icon, icon};
use super::style::{Colors, RADIUS_LG, RADIUS_MD, RADIUS_SM, UI, alpha, mix, weight};
use super::view::{
    dot, empty, heading, kv_list, note, notice_line, rule, section, segmented, small, status_word,
};
use super::*;
use crate::controls::{Kind, button as action, custom, ghost, input, primary, segment};
use agentdocker_core::runtime::Wiring;
use iced::{
    Border, Center, Element, Fill, Font, Length, Padding,
    widget::{Space, column, container, row, scrollable, text},
};

const MEDIUM: Font = weight(iced::font::Weight::Medium);
const SEMIBOLD: Font = weight(iced::font::Weight::Semibold);
type ButtonStyle = iced::widget::button::Style;
type ButtonStatus = iced::widget::button::Status;

/// A string field of a CLI report, or "unknown".
fn field(json: &serde_json::Value, key: &str) -> String {
    json[key].as_str().unwrap_or("unknown").to_owned()
}

/// A path with the home folder as `~`.
fn tilde(path: &std::path::Path) -> String {
    match std::env::var_os("HOME").map(std::path::PathBuf::from) {
        Some(home) => match path.strip_prefix(&home) {
            Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        },
        None => path.display().to_string(),
    }
}

/// Monospace for paths, commands, ids and versions; wraps anywhere, so a
/// long path breaks rather than running off its card.
fn mono<'a>(value: impl Into<String>, size: u32, color: iced::Color) -> iced::widget::Text<'a> {
    text(value.into())
        .size(size)
        .font(Font::MONOSPACE)
        .color(color)
        .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
}

fn padding(top: f32, right: f32, bottom: f32, left: f32) -> Padding {
    Padding {
        top,
        right,
        bottom,
        left,
    }
}

/// A flat fill with a radius and no border.
fn fill_style(
    background: iced::Color,
    radius: impl Into<iced::border::Radius>,
) -> container::Style {
    container::Style {
        background: Some(background.into()),
        border: Border {
            radius: radius.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// A card that holds its children inside its hairline: padding of one,
/// so a tinted header or footer strip does not paint over the border.
fn frame<'a>(content: impl Into<Element<'a, Message>>, c: Colors) -> Element<'a, Message> {
    container(content)
        .padding(1)
        .width(Fill)
        .style(move |_| c.card_style())
        .into()
}

/// A notice that something went wrong: an amber wash, a dot and the words.
fn alert<'a>(words: impl Into<String>, c: Colors) -> Element<'a, Message> {
    container(
        row![
            dot(c.amber, 7.0, c),
            text(words.into()).size(13).color(c.text).width(Fill)
        ]
        .spacing(10)
        .align_y(Center),
    )
    .padding([10, 14])
    .width(Fill)
    .style(move |_| c.attention_style(c.amber))
    .into()
}

/// Rows separated by rules inset to the row padding, for a card body.
fn rows_body<'a>(rows: Vec<Element<'a, Message>>, c: Colors) -> Element<'a, Message> {
    let mut list = column![].width(Fill);
    for (index, item) in rows.into_iter().enumerate() {
        if index > 0 {
            list = list.push(container(rule(c)).padding([0, 16]));
        }
        list = list.push(item);
    }
    list.into()
}

/// A settings row: what it is and one line of why on the left, the
/// control on the right.
fn setting<'a>(
    title_text: impl Into<String>,
    description: impl Into<String>,
    control: impl Into<Element<'a, Message>>,
    c: Colors,
) -> Element<'a, Message> {
    container(
        row![
            column![
                text(title_text.into()).size(14).font(MEDIUM),
                text(description.into()).size(13).color(c.muted)
            ]
            .spacing(2)
            .width(Fill),
            control.into()
        ]
        .spacing(16)
        .align_y(Center),
    )
    .padding([12, 16])
    .width(Fill)
    .into()
}

/// A field row: title and description over the input, for values too
/// long to sit beside their name.
fn field_row<'a>(
    title_text: impl Into<String>,
    description: impl Into<String>,
    control: impl Into<Element<'a, Message>>,
    c: Colors,
) -> Element<'a, Message> {
    container(
        column![
            column![
                text(title_text.into()).size(14).font(MEDIUM),
                text(description.into()).size(13).color(c.muted)
            ]
            .spacing(2),
            control.into()
        ]
        .spacing(10),
    )
    .padding([12, 16])
    .width(Fill)
    .into()
}

/// A focusable, labelled control with a style of its own, for the few
/// controls the shared ladder does not draw: a switch, a stepper's halves,
/// a palette swatch. Same keyboard and accessibility behaviour as every
/// other button.
#[allow(clippy::too_many_arguments)]
fn bare<'a>(
    id: impl Into<String>,
    label: impl Into<String>,
    content: impl Into<Element<'a, Message>>,
    message: Option<Message>,
    pad: impl Into<Padding>,
    width: Length,
    height: Length,
    style: impl Fn(&iced::Theme, ButtonStatus) -> ButtonStyle + 'a,
) -> Element<'a, Message> {
    let (id, label) = (id.into(), label.into());
    let button = iced::widget::button(content)
        .padding(pad)
        .width(width)
        .height(height)
        .on_press_maybe(message.clone())
        .style(style);
    crate::controls::Control {
        content: button.into(),
        semantic: crate::accessibility::Semantic::button(id, label, message),
        button: true,
    }
    .into()
}

/// An on/off switch: a 36 × 20 track, accent when on, with a white thumb
/// at the end that is on. The spoken label says the state in words.
fn switch<'a>(
    id: impl Into<String>,
    label: impl Into<String>,
    on: bool,
    message: Option<Message>,
    c: Colors,
) -> Element<'a, Message> {
    let off_track = if c.dark {
        c.line_strong
    } else {
        mix(c.line_strong, c.muted, 0.25)
    };
    let thumb_fill = if on || !c.dark {
        iced::Color::WHITE
    } else {
        mix(c.muted, c.text, 0.25)
    };
    let enabled = message.is_some();
    let thumb = container(Space::new().width(16).height(16)).style(move |_| {
        c.dot(if enabled {
            thumb_fill
        } else {
            alpha(thumb_fill, 0.7)
        })
    });
    let lane: Element<'a, Message> = if on {
        row![Space::new().width(Fill), thumb].into()
    } else {
        row![thumb, Space::new().width(Fill)].into()
    };
    bare(
        id,
        label,
        lane,
        message,
        2,
        Length::Fixed(36.0),
        Length::Fixed(20.0),
        move |_, status| {
            let base = if on { c.accent } else { off_track };
            let track = match status {
                ButtonStatus::Hovered | ButtonStatus::Pressed if on => c.accent_hover,
                ButtonStatus::Hovered | ButtonStatus::Pressed => mix(off_track, c.text, 0.10),
                ButtonStatus::Disabled => alpha(base, 0.45),
                ButtonStatus::Active => base,
            };
            ButtonStyle {
                background: Some(track.into()),
                text_color: c.text,
                border: Border {
                    radius: 10.0.into(),
                    ..Default::default()
                },
                shadow: Default::default(),
                snap: true,
            }
        },
    )
}

/// A number with a step down and a step up in one bordered control:
/// `−  14 pt  +`. Each half is its own labelled control and goes quiet at
/// its end of the range.
fn stepper<'a>(
    down: (&'static str, &'static str, Option<Message>),
    shown: String,
    up: (&'static str, &'static str, Option<Message>),
    c: Colors,
) -> Element<'a, Message> {
    let half = |(id, label, message): (&'static str, &'static str, Option<Message>),
                glyph: &'static str,
                left: bool| {
        bare(
            id,
            label,
            text(glyph).size(16).font(MEDIUM).center().width(Fill),
            message,
            [5, 0],
            Length::Fixed(32.0),
            Length::Fixed(30.0),
            move |_, status| {
                let radius = RADIUS_SM - 1.0;
                ButtonStyle {
                    background: match status {
                        ButtonStatus::Hovered => Some(alpha(c.text, 0.06).into()),
                        ButtonStatus::Pressed => Some(alpha(c.text, 0.10).into()),
                        _ => None,
                    },
                    text_color: if status == ButtonStatus::Disabled {
                        alpha(c.muted, 0.4)
                    } else {
                        c.text
                    },
                    border: Border {
                        radius: if left {
                            iced::border::left(radius)
                        } else {
                            iced::border::right(radius)
                        },
                        ..Default::default()
                    },
                    shadow: Default::default(),
                    snap: true,
                }
            },
        )
    };
    container(
        row![
            half(down, "−", true),
            text(shown)
                .size(13)
                .font(MEDIUM)
                .center()
                .width(Length::Fixed(52.0)),
            half(up, "+", false)
        ]
        .align_y(Center),
    )
    .padding(1)
    .style(move |_| container::Style {
        background: Some(c.card.into()),
        border: Border {
            color: c.line_strong,
            width: 1.0,
            radius: RADIUS_SM.into(),
        },
        ..Default::default()
    })
    .into()
}

/// A key or a key combination as keycaps: each key on its own small tile.
fn keys<'a>(combination: &[&str], c: Colors) -> Element<'a, Message> {
    let mut caps = row![].spacing(3).align_y(Center);
    for key in combination {
        caps = caps.push(
            container(
                text((*key).to_owned())
                    .size(11)
                    .line_height(iced::Pixels(16.0))
                    .font(MEDIUM)
                    .color(c.muted)
                    .center(),
            )
            .padding([1, 6])
            .width(Length::Shrink)
            .style(move |_| container::Style {
                background: Some(c.raised.into()),
                border: Border {
                    color: c.line_strong,
                    width: 1.0,
                    radius: 5.0.into(),
                },
                ..Default::default()
            }),
        );
    }
    caps.into()
}

/// A disclosure: muted words and a chevron that says which way it opens.
fn disclosure<'a>(
    id: impl Into<String>,
    open: bool,
    message: Option<Message>,
    c: Colors,
) -> Element<'a, Message> {
    let words = if open { "Hide details" } else { "Details" };
    custom(
        id,
        words,
        row![
            text(words)
                .size(13)
                .line_height(iced::Pixels(crate::controls::LABEL_LINE))
                .font(MEDIUM),
            icon(
                if open {
                    Icon::ChevronUp
                } else {
                    Icon::ChevronDown
                },
                c.muted,
                13.0
            )
        ]
        .spacing(4)
        .align_y(Center),
        message,
        false,
        Kind::Ghost,
        [7, 8],
    )
}

/// A tool's mark: its own logo where the app has one (`app/logos.rs`),
/// otherwise two letters on the tool's own tint.
fn tool_mark<'a>(label: &str, seed: &str, c: Colors) -> Element<'a, Message> {
    if let Some(logo) = super::logos::Logo::for_runtime(seed) {
        return super::view::logo_tile(logo, 32.0, c);
    }
    let (tint, ink) = super::style::identity(seed, c.dark);
    let letters = mark_letters(label);
    container(text(letters).size(13).font(SEMIBOLD).color(ink))
        .center(32)
        .style(move |_| fill_style(tint, RADIUS_MD))
        .into()
}

/// The letters on a tool's mark: an acronym it leads with ("VS Code"),
/// the initials of its first two words ("Claude Code"), or the first two
/// letters of a one-word name ("Codex").
fn mark_letters(label: &str) -> String {
    let words: Vec<&str> = label
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    match words.as_slice() {
        [first, ..] if first.len() <= 3 && first.chars().all(char::is_uppercase) => {
            (*first).to_owned()
        }
        [first, second, ..] => first
            .chars()
            .take(1)
            .chain(second.chars().take(1))
            .collect::<String>()
            .to_uppercase(),
        [only] => {
            let mut chars = only.chars();
            let head: String = chars
                .next()
                .into_iter()
                .flat_map(char::to_uppercase)
                .collect();
            let tail: String = chars
                .next()
                .into_iter()
                .flat_map(char::to_lowercase)
                .collect();
            head + &tail
        }
        [] => "·".to_owned(),
    }
}

/// The state of one step in a short sequence.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Done,
    /// The step that is next, or under way.
    Next,
    Waiting,
    /// Taken back.
    Undone,
}

/// A 16-point tile for a step: a tick when done, an accent dot for the
/// one that is next, a small square while waiting, a cross when undone.
fn step_tile<'a>(step: Step, c: Colors) -> Element<'a, Message> {
    let tile = |content: Element<'a, Message>, tone: iced::Color| -> Element<'a, Message> {
        container(content)
            .center(16)
            .style(move |_| fill_style(alpha(tone, if c.dark { 0.18 } else { 0.14 }), 5.0))
            .into()
    };
    match step {
        Step::Done => tile(icon(Icon::Check, c.green, 11.0), c.green),
        Step::Next => tile(dot(c.accent, 6.0, c), c.accent),
        Step::Undone => tile(icon(Icon::Close, c.faint, 10.0), c.faint),
        Step::Waiting => container(
            container(Space::new().width(5).height(5))
                .style(move |_| fill_style(c.line_strong, 2.0)),
        )
        .center(16)
        .into(),
    }
}

/// One row of a step list: its tile, its words, and a quiet fact at the
/// right in monospace.
fn step_row<'a>(
    step: Step,
    label: impl Into<String>,
    meta: Option<String>,
    c: Colors,
) -> Element<'a, Message> {
    let ink = match step {
        Step::Done => c.muted,
        Step::Next => c.text,
        Step::Waiting | Step::Undone => c.faint,
    };
    let mut line = row![
        step_tile(step, c),
        text(label.into())
            .size(13)
            .color(ink)
            .font(if step == Step::Next { MEDIUM } else { UI })
            .width(Fill)
    ]
    .spacing(10)
    .align_y(Center);
    if let Some(meta) = meta {
        line = line.push(mono(meta, 11, c.faint));
    }
    container(line).height(28).center_y(28).into()
}

/// Words for how a tool's integration is wired, and their tone.
fn wiring_word(wiring: Wiring, c: Colors) -> (&'static str, iced::Color) {
    match wiring {
        Wiring::Wired => ("Configured", c.green),
        Wiring::Unverified => ("Configured, unverified", c.amber),
        Wiring::Missing => ("Not configured", c.amber),
        Wiring::Unsupported => ("Not available", c.faint),
    }
}

/// The tone that goes with a session's input readiness.
fn readiness_tone(word: &str, c: Colors) -> iced::Color {
    match word {
        "Receiving messages" | "Ready for messages" => c.green,
        "Sent · waiting for the agent to take it" => c.accent,
        "Messages may wait for its next prompt" => c.cyan,
        "Session ended" | "Readiness unavailable" => c.faint,
        _ => c.amber,
    }
}

/// The colour a terminal palette draws its prompt in: its yellow, the
/// bright one on a dark ground.
fn prompt_ink(palette: &crate::theme::Palette) -> iced::Color {
    let ground: iced::Color = palette.ground.into();
    let dark = 0.299 * ground.r + 0.587 * ground.g + 0.114 * ground.b < 0.5;
    palette.ansi[if dark { 11 } else { 3 }].into()
}

/// What one tool's row needs to know, worked out once for the row and its
/// details.
struct ToolState<'a> {
    expanded: bool,
    installed: bool,
    reporting: bool,
    supported: bool,
    unverified: bool,
    missing: bool,
    ready: bool,
    sessions: Vec<&'a AgentRecord>,
    tone: iced::Color,
    word: String,
}

impl App {
    fn tool_state<'a>(&'a self, runtime: &RuntimeInfo, c: Colors) -> ToolState<'a> {
        let expanded = self.shell.connection_details.as_deref() == Some(runtime.name.as_str());
        let installed = runtime.installed();
        let reporting = self.tool_reports(&runtime.name);
        let supported = runtime.mcp != Wiring::Unsupported || runtime.hooks != Wiring::Unsupported;
        let unverified = runtime.mcp == Wiring::Unverified || runtime.hooks == Wiring::Unverified;
        let sessions: Vec<_> = self
            .agents
            .iter()
            .filter(|agent| agent.spec.runtime == runtime.name && self.live_session(agent))
            .collect();
        let ready = self.connected.is_ok()
            && sessions.iter().any(|agent| {
                agent.input_delivery.as_ref().is_some_and(|delivery| {
                    delivery.current_for(agent.process_started_at, Utc::now())
                })
            });
        let missing =
            installed && (runtime.mcp == Wiring::Missing || runtime.hooks == Wiring::Missing);
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
        let (tone, word) = if ready {
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
        } else if agentdocker_core::runtime::spec(&runtime.name)
            .is_some_and(|spec| spec.mcp == agentdocker_core::runtime::McpWiring::AgentConfig)
        {
            // Nothing for setup to write: each agent's YAML lists the toolset.
            (c.cyan, "Installed · wired in each agent's YAML".to_owned())
        } else if !supported {
            (c.faint, "Installed · integration unavailable".to_owned())
        } else if unverified {
            (c.amber, "Setup needs review".to_owned())
        } else if missing {
            (c.amber, super::missing_setup(runtime))
        } else {
            (c.cyan, "Configured · waiting for contact".to_owned())
        };
        ToolState {
            expanded,
            installed,
            reporting,
            supported,
            unverified,
            missing,
            ready,
            sessions,
            tone,
            word,
        }
    }

    /// Tools: the setup under review and the connection check when one is
    /// open, then one card with a row per tool.
    pub(super) fn connections(&self, c: Colors) -> Element<'_, Message> {
        let mut page = column![].spacing(16).width(Fill);
        if self.setup_busy {
            page = page.push(notice_line("Checking…", None, c.accent, c));
        }
        if let Some(error) = &self.shell.setup_error {
            page = page.push(alert(error.clone(), c));
        }
        if let Some(plan) = &self.setup_plan {
            page = page.push(self.setup_view(plan, c));
        }
        if let Some(health) = &self.setup_health {
            page = page.push(self.health_view(health, c));
        }
        let mut runtimes: Vec<_> = self
            .runtimes
            .iter()
            .filter(|r| self.shell.other_tools || r.installed())
            .collect();
        runtimes.sort_by_key(|r| !r.installed());
        let other_count = self.runtimes.iter().filter(|r| !r.installed()).count();
        if runtimes.is_empty() {
            page = page.push(empty(
                "No supported tools found",
                "Install Claude Code or Codex, then come back here.",
                None,
                c,
            ));
        }
        // One filled action on the screen: the first tool that needs
        // setting up; the others' Set up step down to an outline, and all
        // of them do while a setup is under review (its Connect leads).
        let mut led = self.setup_plan.is_some();
        let mut rows: Vec<Element<'_, Message>> = runtimes
            .into_iter()
            .map(|runtime| {
                let needs = self.tool_needs_action(runtime, c);
                let lead = needs && !led;
                led |= needs;
                self.tool_row(runtime, lead, c)
            })
            .collect();
        if other_count > 0 {
            let words = if self.shell.other_tools {
                "Hide other tools".to_owned()
            } else {
                format!("Other supported tools ({other_count})")
            };
            rows.push(
                container(custom(
                    "other-tools",
                    words.clone(),
                    row![
                        text(words)
                            .size(13)
                            .line_height(iced::Pixels(crate::controls::LABEL_LINE))
                            .font(MEDIUM)
                            .color(c.muted),
                        icon(
                            if self.shell.other_tools {
                                Icon::ChevronUp
                            } else {
                                Icon::ChevronDown
                            },
                            c.muted,
                            13.0
                        )
                    ]
                    .spacing(6)
                    .align_y(Center),
                    Some(Message::OtherTools),
                    false,
                    Kind::Ghost,
                    [7, 10],
                ))
                .padding([6, 6])
                .into(),
            );
        }
        if !rows.is_empty() {
            let mut list = column![].width(Fill);
            for (index, item) in rows.into_iter().enumerate() {
                if index > 0 {
                    list = list.push(rule(c));
                }
                list = list.push(item);
            }
            page = page.push(frame(list, c));
        }
        if !self.discovered.is_empty() {
            page = page.push(action(
                "register-all-discovered",
                format!("Connect all {} running sessions", self.discovered.len()),
                self.connected.is_ok().then_some(Message::AdoptAll),
                false,
            ));
        }
        if !self.setup_history.is_empty() {
            let rows: Vec<Element<'_, Message>> = self
                .setup_history
                .iter()
                .map(|plan| -> Element<'_, Message> {
                    let id = field(plan, "id");
                    let phase = field(plan, "phase");
                    let tone = match phase.as_str() {
                        "applied" => c.green,
                        "prepared" | "applying" => c.accent,
                        _ => c.faint,
                    };
                    container(custom(
                        format!("saved-setup-{id}"),
                        format!("{phase} · {id}"),
                        row![
                            mono(id.clone(), 12, c.text).width(Fill),
                            status_word(phase.clone(), tone, c),
                            icon(Icon::ChevronRight, c.faint, 13.0)
                        ]
                        .spacing(12)
                        .align_y(Center),
                        (!self.setup_busy).then_some(Message::Setup(vec!["--show".into(), id])),
                        false,
                        Kind::Quiet,
                        [9, 10],
                    ))
                    .padding([2, 6])
                    .into()
                })
                .collect();
            page = page.push(section(
                "Setup history",
                Some("Setups saved on this machine. Open one to see it again.".to_owned()),
                None,
                column(rows),
                c,
            ));
        }
        page.into()
    }

    /// Whether a tool's row offers Set up.
    fn tool_needs_action(&self, runtime: &RuntimeInfo, c: Colors) -> bool {
        let state = self.tool_state(runtime, c);
        state.installed
            && state.supported
            && (state.missing || state.unverified)
            && !state.reporting
    }

    /// One tool: its mark, its name over its status, and one thing to do
    /// at the right — Set up when it needs it, filled only for the `lead`
    /// tool — then Details.
    fn tool_row<'a>(
        &'a self,
        runtime: &'a RuntimeInfo,
        lead: bool,
        c: Colors,
    ) -> Element<'a, Message> {
        let narrow = self.narrow();
        let state = self.tool_state(runtime, c);
        let version = runtime
            .version
            .as_deref()
            .map(|v| format!(" · {v}"))
            .unwrap_or_default();
        let needs_action = self.tool_needs_action(runtime, c);
        // A tool taking messages says so at the right, where a button
        // would otherwise be; every other state is a dot and its words
        // under the name.
        let status: Element<'a, Message> = if state.ready {
            text(format!("{}{version}", state.word))
                .size(12)
                .color(c.muted)
                .width(Fill)
                .into()
        } else {
            row![
                dot(state.tone, 7.0, c),
                text(format!("{}{version}", state.word))
                    .size(12)
                    .color(c.muted)
                    .width(Fill)
            ]
            .spacing(7)
            .align_y(Center)
            .into()
        };
        let mut trailing = row![].spacing(6).align_y(Center);
        if needs_action {
            let id = format!("setup-{}", runtime.name);
            let press = (!self.setup_busy).then_some(Message::Setup(vec![
                runtime.name.clone(),
                "--preview".into(),
            ]));
            trailing = trailing.push(if lead {
                primary(id, "Set up", press)
            } else {
                action(id, "Set up", press, false)
            });
        } else if state.ready {
            trailing =
                trailing.push(container(status_word("Connected", c.green, c)).padding([0, 6]));
        }
        trailing = trailing.push(disclosure(
            format!("connection-details-{}", runtime.name),
            state.expanded,
            Some(Message::ConnectionDetails(runtime.name.clone())),
            c,
        ));
        let mut head = row![].spacing(12).align_y(Center);
        if !narrow {
            head = head.push(tool_mark(&runtime.label, &runtime.name, c));
        }
        head = head.push(
            column![text(runtime.label.clone()).size(14).font(MEDIUM), status]
                .spacing(3)
                .width(Fill),
        );
        head = head.push(trailing);
        let mut block = column![container(head).padding([12, 16]).width(Fill)].width(Fill);
        if state.expanded {
            block = block.push(
                container(self.tool_details(runtime, &state, c))
                    .padding(padding(2.0, 16.0, 18.0, if narrow { 16.0 } else { 60.0 }))
                    .width(Fill),
            );
        }
        block.into()
    }

    /// A tool's details: one line per check, the facts as a definition
    /// list, its live sessions, and the three ways to look further.
    fn tool_details<'a>(
        &'a self,
        runtime: &'a RuntimeInfo,
        state: &ToolState<'a>,
        c: Colors,
    ) -> Element<'a, Message> {
        // One definition list: how each piece is wired, as a dot and a
        // word, then the facts about the tool itself.
        let wired = |wiring: Wiring| -> Element<'a, Message> {
            let (word, tone) = wiring_word(wiring, c);
            status_word(word, tone, c)
        };
        let mut facts: Vec<(String, Element<'a, Message>)> = vec![
            ("Tools (MCP)".into(), wired(runtime.mcp)),
            ("Live activity (hooks)".into(), wired(runtime.hooks)),
        ];
        if runtime.name == "claude-code" {
            let (word, tone, why) = match runtime.shell {
                Wiring::Wired => (
                    "Wake on messages",
                    c.green,
                    "Your shell startup file carries the channel flag, so a `claude` started in a terminal wakes when a message arrives.",
                ),
                Wiring::Missing => (
                    "At their next prompt",
                    c.amber,
                    "A `claude` started in a terminal sees messages at its next prompt. Wake terminal sessions adds the channel flag to every `claude`.",
                ),
                Wiring::Unverified => (
                    "Older setup",
                    c.amber,
                    "An older AgentDocker block is in your shell startup file. Wake terminal sessions replaces it.",
                ),
                Wiring::Unsupported => (
                    "Shell not supported",
                    c.faint,
                    "Setup knows zsh, bash and fish. Start claude with --dangerously-load-development-channels server:agentdocker yourself.",
                ),
            };
            let mut value = column![status_word(word, tone, c), small(why, c)].spacing(4);
            // A `claude` typed in a terminal only sees messages at its next
            // prompt unless the shell adds the channel flag; one reviewed
            // change does that, and this is where the person looks for it.
            if state.installed && matches!(runtime.shell, Wiring::Missing | Wiring::Unverified) {
                value = value.push(
                    container(action(
                        "setup-shell",
                        "Wake terminal sessions",
                        (!self.setup_busy)
                            .then_some(Message::Setup(vec!["--shell".into(), "--preview".into()])),
                        false,
                    ))
                    .padding(padding(4.0, 0.0, 2.0, 0.0)),
                );
            }
            facts.push(("Terminal launches".into(), value.into()));
        }
        facts.extend([
            (
                "Vendor".into(),
                text(runtime.vendor.to_string()).size(13).into(),
            ),
            (
                "Version".into(),
                text(
                    runtime
                        .version
                        .as_deref()
                        .unwrap_or("Version unknown")
                        .to_owned(),
                )
                .size(13)
                .into(),
            ),
            (
                "Command".into(),
                match &runtime.cli {
                    Some(path) => mono(tilde(path), 12, c.text).into(),
                    None => text("No command-line tool found")
                        .size(13)
                        .color(c.muted)
                        .into(),
                },
            ),
        ]);
        for app in &runtime.apps {
            facts.push((
                "Application".into(),
                text(app.label.clone()).size(13).into(),
            ));
        }
        for extension in &runtime.extensions {
            facts.push((
                "Browser".into(),
                text(super::extension_words(extension)).size(13).into(),
            ));
            if let Some(bridge) = &extension.bridge {
                facts.push(("Bridge".into(), mono(tilde(bridge), 12, c.text).into()));
            }
        }
        let mut notes: Vec<Element<'a, Message>> = Vec::new();
        if runtime.in_browser() && state.installed {
            notes.push(Element::from(note(
                agentdocker_core::runtime::IN_BROWSER,
                c,
            )));
            // The connector is what brings a browser agent here: its
            // address and pairing code are what the person needs at the
            // vendor's settings and on the consent page, and this card is
            // where they look for them.
            match &self.connector {
                Some(serving) => {
                    facts.push((
                        "Connector".into(),
                        mono(serving.mcp_url(), 12, c.text).into(),
                    ));
                    facts.push((
                        "Pairing code".into(),
                        column![
                            mono(serving.pairing_code.clone(), 13, c.text),
                            small(
                                "Typed on the consent page, which also asks which project the agent joins",
                                c
                            )
                        ]
                        .spacing(2)
                        .into(),
                    ));
                    facts.push((
                        "Add it".into(),
                        text(super::add_connector_words(&runtime.name))
                            .size(13)
                            .into(),
                    ));
                }
                None => {
                    facts.push(("Connector".into(), status_word("Not running", c.faint, c)));
                    if cfg!(target_os = "macos") || cfg!(target_os = "linux") {
                        notes.push(Element::from(note(
                            "Enable a connection for Claude and ChatGPT. It starts at login and uses a public HTTPS tunnel; each browser account still needs your consent. Existing service settings are preserved.", c)));
                        notes.push(Element::from(note(
                            "Tailscale keeps the same address and needs Funnel enabled. Cloudflare gives a new address after each restart, so saved browser connections must be added again.", c)));
                        let busy = |tunnel| self.connector_busy == Some(tunnel);
                        notes.push(Element::from(
                            row![
                                action(
                                    format!("connector-tailscale-{}", runtime.name),
                                    if busy(super::ConnectorTunnel::Tailscale) {
                                        "Starting…"
                                    } else {
                                        "Enable with Tailscale"
                                    },
                                    self.connector_busy.is_none().then_some(
                                        Message::ConnectorEnable(super::ConnectorTunnel::Tailscale)
                                    ),
                                    false
                                ),
                                action(
                                    format!("connector-cloudflare-{}", runtime.name),
                                    if busy(super::ConnectorTunnel::Cloudflared) {
                                        "Starting…"
                                    } else {
                                        "Enable with Cloudflare"
                                    },
                                    self.connector_busy.is_none().then_some(
                                        Message::ConnectorEnable(
                                            super::ConnectorTunnel::Cloudflared
                                        )
                                    ),
                                    false
                                ),
                            ]
                            .spacing(8)
                            .wrap(),
                        ));
                    } else {
                        notes.push(Element::from(note("Browser connector login services are currently available on macOS and Linux.", c)));
                    }
                    if let Some(error) = &self.connector_error {
                        notes.push(Element::from(alert(error.clone(), c)));
                    }
                }
            }
        }
        for why in &runtime.incomplete {
            notes.push(Element::from(note(
                format!("Inventory incomplete: {why}"),
                c,
            )));
        }
        if state.installed
            && state.supported
            && !state.missing
            && !state.unverified
            && !state.reporting
        {
            notes.push(Element::from(note(
                "Setup is saved. Start a fresh session to load it. Approve only the \
                 integration prompts shown by the provider.",
                c,
            )));
        }
        if !state.sessions.is_empty()
            && !state.ready
            && matches!(runtime.name.as_str(), "codex" | "claude-code")
            && state.sessions.iter().any(|agent| {
                agent.input_delivery.as_ref().is_none_or(|delivery| {
                    Some(delivery.process_started_at) != agent.process_started_at
                })
            })
        {
            notes.push(Element::from(note(
                if runtime.name == "claude-code" {
                    "Hooks cannot start an idle turn. New Claude launches use Idle messages: On and require channel consent. Existing sessions need a safe reconnect; queued messages stay with their current record."
                } else {
                    "Hooks cannot start an idle turn. Codex sessions need their message delivery connected. New launches here use Idle messages: On."
                },
                c,
            )));
        }
        let mut details = column![kv_list(facts, c)].spacing(16).width(Fill);
        if let Some(spec) = agentdocker_core::runtime::spec(&runtime.name) {
            details = details.push(self.capabilities_list(runtime, spec, c));
        }
        if !state.sessions.is_empty() {
            let mut sessions = column![super::view::eyebrow("Sessions", c)].spacing(10);
            let now = Utc::now();
            for agent in &state.sessions {
                let seen = |kind| {
                    agent.adapter_contacts.get(&kind).is_some_and(|contact| {
                        self.connected.is_ok() && contact.current_for(agent.process_started_at, now)
                    })
                };
                let contact = |name: &str, seen: bool| {
                    status_word(
                        format!(
                            "{name}: {}",
                            if seen {
                                "recent contact"
                            } else {
                                "no recent contact"
                            }
                        ),
                        if seen { c.green } else { c.faint },
                        c,
                    )
                };
                let readiness = self.input_readiness(agent);
                let mut entry = column![
                    row![
                        text(self.display_name(agent))
                            .size(13)
                            .font(MEDIUM)
                            .width(Fill),
                        status_word(readiness, readiness_tone(readiness, c), c)
                    ]
                    .spacing(12)
                    .align_y(Center),
                    row![
                        contact("MCP", seen(agentdocker_core::AdapterKind::Mcp)),
                        contact("Hooks", seen(agentdocker_core::AdapterKind::Hooks))
                    ]
                    .spacing(16)
                    .wrap()
                ]
                .spacing(6);
                if let Some(guidance) =
                    super::send_readiness::reconnect(agent, &self.agents, "tools", c)
                {
                    entry = entry.push(guidance);
                }
                sessions = sessions.push(container(entry).padding([10, 12]).width(Fill).style(
                    move |_| container::Style {
                        background: Some(mix(c.card, c.raised, 0.45).into()),
                        border: Border {
                            color: c.line,
                            width: 1.0,
                            radius: RADIUS_MD.into(),
                        },
                        ..Default::default()
                    },
                ));
            }
            details = details.push(sessions);
        }
        if !notes.is_empty() {
            details = details.push(column(notes).spacing(8).width(Fill));
        }
        details = details.push(
            row![
                action(
                    format!("setup-review-{}", runtime.name),
                    "Review setup",
                    (!self.setup_busy && state.supported).then_some(Message::Setup(vec![
                        runtime.name.clone(),
                        "--preview".into(),
                    ])),
                    false
                ),
                ghost(
                    "connection-health",
                    "Check connections",
                    (!self.setup_busy).then_some(Message::Setup(vec!["--health".into()])),
                ),
                ghost(
                    "saved-setups",
                    "Setup history",
                    (!self.setup_busy).then_some(Message::Setup(vec!["--list".into()])),
                )
            ]
            .spacing(6)
            .wrap(),
        );
        details.into()
    }

    /// What AgentDocker can do for this tool's sessions, one line per
    /// capability: how far it reaches, whether what it runs through is set
    /// up here, and a sentence of how.
    fn capabilities_list<'a>(
        &self,
        runtime: &RuntimeInfo,
        spec: &agentdocker_core::runtime::RuntimeSpec,
        c: Colors,
    ) -> Element<'a, Message> {
        use agentdocker_core::runtime::Reach;
        let mut rows: Vec<(String, Element<'a, Message>)> = Vec::new();
        for (_, label, capability) in spec.capabilities().rows() {
            let pending = capability
                .installed(runtime)
                .is_some_and(|wiring| wiring != Wiring::Wired);
            let (word, tone) = match capability.reach {
                Reach::Automatic if pending => ("Automatic once set up", c.amber),
                Reach::Automatic => ("Automatic", c.green),
                Reach::Voluntary => ("When the agent uses the tools", c.cyan),
                Reach::Conditional => ("With setup at launch", c.amber),
                Reach::Unavailable => ("Not available", c.faint),
            };
            rows.push((
                label.to_owned(),
                column![status_word(word, tone, c), small(capability.how, c)]
                    .spacing(4)
                    .into(),
            ));
        }
        let mut list = column![
            super::view::eyebrow("What AgentDocker can do", c),
            kv_list(rows, c)
        ]
        .spacing(10);
        if spec.mcp == agentdocker_core::runtime::McpWiring::AgentConfig {
            list = list.push(note(
                format!(
                    "{} reads MCP servers from each agent's YAML. `agentdocker setup {}` prints the toolset to add; each run that lists it joins as one agent.",
                    runtime.label, runtime.name
                ),
                c,
            ));
        }
        list.into()
    }

    /// The connection check, in words: one line per installed tool with
    /// what needs attention under it, and a way to put it away.
    pub(super) fn health_view(
        &self,
        health: &serde_json::Value,
        c: Colors,
    ) -> Element<'_, Message> {
        let installed: BTreeSet<&str> = self
            .runtimes
            .iter()
            .filter(|r| r.installed())
            .map(|r| r.name.as_str())
            .collect();
        let mut rows = Vec::new();
        for runtime in health["runtimes"].as_array().into_iter().flatten() {
            let name = field(runtime, "name");
            if !installed.contains(name.as_str()) {
                continue;
            }
            let label = self
                .runtimes
                .iter()
                .find(|r| r.name == name)
                .map(|r| r.label.clone())
                .unwrap_or(name);
            let mut problems = Vec::new();
            for check in runtime["checks"].as_array().into_iter().flatten() {
                let status = field(check, "status");
                if matches!(
                    status.as_str(),
                    "ok" | "executable_available" | "unsupported"
                ) {
                    continue;
                }
                problems.push(format!(
                    "{}: {}",
                    field(check, "channel"),
                    field(check, "detail")
                ));
            }
            let ok = problems.is_empty();
            let mut line = column![
                row![
                    text(label).size(14).font(MEDIUM).width(Fill),
                    if ok {
                        status_word("Configuration checked", c.green, c)
                    } else {
                        status_word("Needs attention", c.amber, c)
                    }
                ]
                .spacing(12)
                .align_y(Center)
            ]
            .spacing(4);
            for problem in problems {
                line = line.push(small(problem, c));
            }
            rows.push(container(line).padding([10, 16]).width(Fill).into());
        }
        if rows.is_empty() {
            rows.push(
                container(note("No installed tool to check.", c))
                    .padding([10, 16])
                    .into(),
            );
        }
        section(
            "Connection check",
            Some(field(health, "daemon")),
            Some(ghost("close-health", "Close", Some(Message::SetupClose))),
            rows_body(rows, c),
            c,
        )
    }

    /// One setup plan as a reviewed change: what it is for, the steps from
    /// preview to a session that has loaded it, each file it touches as a
    /// diff of the entries it adds or takes back, and one Connect.
    pub(super) fn setup_view(&self, plan: &serde_json::Value, c: Colors) -> Element<'_, Message> {
        let plan_id = plan["id"].as_str().filter(|id| !id.is_empty());
        let id = plan_id.unwrap_or("unknown").to_owned();
        let phase = field(plan, "phase");
        let changes: Vec<&serde_json::Value> =
            plan["changes"].as_array().into_iter().flatten().collect();
        let tool = changes
            .first()
            .map(|change| field(change, "runtime"))
            .and_then(|name| self.runtimes.iter().find(|r| r.name == name))
            .map(|r| r.label.clone());
        let shell_plan = changes
            .iter()
            .any(|change| field(change, "channel") == "shell");
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
        let applied = phase == "applied";
        let undone = phase == "undone";
        let any = !changes.is_empty();
        // The files, in the plan's order, each with the entries it gains.
        let mut files: Vec<(String, Vec<String>)> = Vec::new();
        for change in &changes {
            let what = match field(change, "channel").as_str() {
                "mcp" => "Tools (MCP)".to_owned(),
                "activity hooks" | "hooks" => "Live activity (hooks)".to_owned(),
                "shell" => "Terminal launches wake (shell startup file)".to_owned(),
                other => other.to_owned(),
            };
            let line = format!("{what}: {}", field(change, "action"));
            let path = change["path"]
                .as_str()
                .map(|p| tilde(std::path::Path::new(p)))
                .unwrap_or_else(|| "unknown".to_owned());
            match files.iter_mut().find(|(p, _)| *p == path) {
                Some((_, lines)) => lines.push(line),
                None => files.push((path, vec![line])),
            }
        }
        let summary = if any {
            format!(
                "{} {} in {} {}",
                changes.len(),
                if changes.len() == 1 {
                    "change"
                } else {
                    "changes"
                },
                files.len(),
                if files.len() == 1 { "file" } else { "files" }
            )
        } else {
            "Everything this tool needs is already in place.".to_owned()
        };
        let header = container(
            row![
                container(icon(Icon::File, c.muted, 16.0))
                    .center(32)
                    .style(move |_| c.tile(RADIUS_MD)),
                column![heading(title_text, 15), small(summary, c)]
                    .spacing(2)
                    .width(Fill)
            ]
            .spacing(12)
            .align_y(Center),
        )
        .padding([12, 15]);
        let mut body = column![].spacing(14).width(Fill);
        if any {
            let (apply_step, load_step) = match phase.as_str() {
                "applied" => (Step::Done, Step::Next),
                "undone" => (Step::Undone, Step::Waiting),
                _ => (Step::Next, Step::Waiting),
            };
            body = body.push(
                column![
                    step_row(Step::Done, "Preview what changes", None, c),
                    step_row(
                        apply_step,
                        match phase.as_str() {
                            "applied" => "Saved",
                            "undone" => "Taken back",
                            "applying" => "Applying…",
                            _ => "Connect to save it",
                        },
                        applied.then(|| short_id(&id)),
                        c
                    ),
                    step_row(
                        load_step,
                        if shell_plan {
                            "Open a new terminal to load it"
                        } else {
                            "Start a fresh session to load it"
                        },
                        None,
                        c
                    ),
                ]
                .spacing(2),
            );
            for (path, lines) in files {
                body = body.push(diff_block(path, lines, undone, c));
            }
        }
        let expanded = self.shell.connection_details.as_deref() == Some("setup-plan");
        if expanded {
            let mut facts: Vec<(String, Element<'_, Message>)> = vec![
                ("Plan".into(), mono(id.clone(), 12, c.text).into()),
                ("State".into(), text(phase.clone()).size(13).into()),
            ];
            if let Some(executable) = plan["executable"].as_str() {
                facts.push(("Command".into(), mono(executable, 12, c.text).into()));
            }
            for change in &changes {
                facts.push((
                    "File".into(),
                    mono(field(change, "path"), 12, c.text).into(),
                ));
            }
            let mut technical = column![kv_list(facts, c)].spacing(8);
            for note_text in plan["notes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s.as_str())
            {
                technical = technical.push(small(note_text, c));
            }
            body = body.push(technical);
        }
        let applicable = !self.setup_busy
            && matches!(phase.as_str(), "prepared" | "applying")
            && any
            && plan_id.is_some();
        let (state_word, tone) = match phase.as_str() {
            "applied" => ("Saved", c.green),
            "undone" => ("Undone", c.faint),
            "applying" => ("Applying…", c.accent),
            _ if !any => ("Nothing to change", c.faint),
            _ => ("Waiting for you", c.amber),
        };
        let mut buttons = row![
            disclosure(
                "setup-details",
                expanded,
                Some(Message::ConnectionDetails("setup-plan".into())),
                c
            ),
            ghost(
                "close-setup",
                "Close",
                (!self.setup_busy).then_some(Message::SetupClose),
            )
        ]
        .spacing(4)
        .align_y(Center);
        if applied && plan_id.is_some() {
            buttons = buttons.push(action(
                "undo-setup",
                "Undo",
                (!self.setup_busy).then_some(Message::Setup(vec!["--undo".into(), id.clone()])),
                false,
            ));
        }
        if applicable {
            buttons = buttons.push(primary(
                "apply-setup",
                "Connect",
                Some(Message::Setup(vec!["--apply".into(), id.clone()])),
            ));
        }
        let footer = container(
            row![
                container(status_word(state_word, tone, c)).width(Fill),
                buttons
            ]
            .spacing(12)
            .align_y(Center),
        )
        .padding([9, 12])
        .width(Fill)
        .style(move |_| {
            fill_style(
                mix(c.card, c.raised, 0.5),
                iced::border::bottom(RADIUS_LG - 1.0),
            )
        });
        let mut card = column![header, rule(c)].width(Fill);
        if any || expanded {
            card = card.push(container(body).padding([14, 16]));
        }
        frame(card.push(rule(c)).push(footer), c)
    }

    /// Settings: one card per section, a row per setting with the control
    /// at its right.
    pub(super) fn settings_view(&self, c: Colors) -> Element<'_, Message> {
        let s = &self.settings;
        let appearance = section(
            "Appearance",
            None,
            None,
            rows_body(
                vec![
                    setting(
                        "Theme",
                        "Light or dark. The rail stays navy in both.",
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
                            c,
                        ),
                        c,
                    ),
                    setting(
                        "Text size",
                        "Every word in the window.",
                        stepper(
                            (
                                "smaller-ui",
                                "Smaller text",
                                (s.text_size > 10.0)
                                    .then_some(Message::TextSize(s.text_size - 1.0)),
                            ),
                            format!("{:.0} pt", s.text_size),
                            (
                                "larger-ui",
                                "Larger text",
                                (s.text_size < 24.0)
                                    .then_some(Message::TextSize(s.text_size + 1.0)),
                            ),
                            c,
                        ),
                        c,
                    ),
                    setting(
                        "Roomier rows",
                        "More space between rows, for a window you watch rather than read.",
                        switch(
                            "roomy-rows",
                            if s.roomy {
                                "Use compact rows"
                            } else {
                                "Use roomier rows"
                            },
                            s.roomy,
                            Some(Message::Roomy(!s.roomy)),
                            c,
                        ),
                        c,
                    ),
                ],
                c,
            ),
            c,
        );
        let mut swatches = row![].spacing(14);
        for palette in crate::theme::PALETTES {
            swatches = swatches.push(self.palette_swatch(palette, c));
        }
        let terminal = section(
            "Terminal",
            None,
            None,
            rows_body(
                vec![
                    setting(
                        "Text size",
                        "Agent terminals and command output.",
                        stepper(
                            (
                                "smaller-terminal",
                                "Smaller terminal text",
                                (s.terminal_size > 9.0)
                                    .then_some(Message::TerminalSize(s.terminal_size - 1.0)),
                            ),
                            format!("{:.0} pt", s.terminal_size),
                            (
                                "larger-terminal",
                                "Larger terminal text",
                                (s.terminal_size < 24.0)
                                    .then_some(Message::TerminalSize(s.terminal_size + 1.0)),
                            ),
                            c,
                        ),
                        c,
                    ),
                    field_row(
                        "Palette",
                        "The colours agent terminals and command output are drawn in.",
                        swatches.wrap().vertical_spacing(12),
                        c,
                    ),
                ],
                c,
            ),
            c,
        );
        let mac = cfg!(target_os = "macos");
        let shortcut =
            |what: &'static str, combos: Vec<Vec<&'static str>>| -> Element<'_, Message> {
                let mut alternatives = row![].spacing(6).align_y(Center);
                for (index, combo) in combos.iter().enumerate() {
                    if index > 0 {
                        alternatives = alternatives.push(text("or").size(12).color(c.faint));
                    }
                    alternatives = alternatives.push(keys(combo, c));
                }
                container(
                    row![text(what).size(14).width(Fill), alternatives]
                        .spacing(16)
                        .align_y(Center),
                )
                .padding([10, 16])
                .width(Fill)
                .into()
            };
        let keyboard = section(
            "Keyboard and accessibility",
            Some("Every control is reachable from the keyboard. Text fields support native input methods.".to_owned()),
            None,
            rows_body(
                vec![
                    shortcut(
                        "Jump to a place, project or action",
                        vec![if mac { vec!["⌘", "K"] } else { vec!["Ctrl", "K"] }],
                    ),
                    shortcut("Move between controls", vec![vec!["Tab"], vec!["⇧", "Tab"]]),
                    shortcut("Activate the focused control", vec![vec!["Enter"], vec!["Space"]]),
                    shortcut(
                        "Switch sections",
                        vec![if mac { vec!["⌘", "1–4"] } else { vec!["Ctrl", "1–4"] }],
                    ),
                    shortcut("Close details or a draft launch form", vec![vec!["Esc"]]),
                    shortcut("Leave terminal input", vec![vec!["F6"]]),
                    shortcut(
                        "Detach the terminal",
                        vec![if mac { vec!["⌃", "]"] } else { vec!["Ctrl", "]"] }],
                    ),
                    shortcut(
                        "Copy the terminal selection",
                        if mac {
                            vec![vec!["⌘", "C"]]
                        } else {
                            vec![vec!["Ctrl", "⇧", "C"]]
                        },
                    ),
                ],
                c,
            ),
            c,
        );
        let mut installation_rows = vec![
            setting(
                "Daily update checks",
                "Looks for a newer release once a day. Nothing downloads or installs on its own.",
                switch(
                    "automatic-update-checks",
                    if self.shell.catalog.updates.enabled {
                        "Daily update checks: on"
                    } else {
                        "Daily update checks: off"
                    },
                    self.shell.catalog.updates.enabled,
                    self.shell.save_enabled.then_some(Message::AutomaticUpdates(
                        !self.shell.catalog.updates.enabled,
                    )),
                    c,
                ),
                c,
            ),
            setting(
                "Installation and versions",
                "Preview and apply installs, rollbacks and cleanup.",
                custom(
                    "installation",
                    "Manage installation and retained versions",
                    row![
                        text("Manage")
                            .size(13)
                            .line_height(iced::Pixels(crate::controls::LABEL_LINE))
                            .font(MEDIUM),
                        icon(Icon::ChevronRight, c.muted, 13.0)
                    ]
                    .spacing(4)
                    .align_y(Center),
                    Some(Message::Navigate(Screen::Desktop)),
                    false,
                    Kind::Secondary,
                    [7, 10],
                ),
                c,
            ),
        ];
        if self.desktop.update_check_error {
            installation_rows.push(
                container(notice_line(
                    "Couldn’t check for updates.",
                    Some("Try again in Installation.".to_owned()),
                    c.amber,
                    c,
                ))
                .padding([10, 16])
                .into(),
            );
        }
        let installation = section(
            "Installation",
            None,
            None,
            rows_body(installation_rows, c),
            c,
        );
        let diagnostics = section(
            "Diagnostics",
            None,
            None,
            container(kv_list(
                vec![
                    (
                        "Local daemon".to_owned(),
                        mono(self.socket.clone(), 12, c.text).into(),
                    ),
                    (
                        "Preferences".to_owned(),
                        mono(tilde(&self.home.join("workspace.json")), 12, c.text).into(),
                    ),
                ],
                c,
            ))
            .padding([12, 16]),
            c,
        );
        column![appearance, terminal, keyboard, installation, diagnostics]
            .spacing(16)
            .into()
    }

    /// A terminal palette as a swatch: a small preview drawn in its own
    /// ground, prompt and colours, ringed in the accent when chosen.
    fn palette_swatch(
        &self,
        palette: &'static crate::theme::Palette,
        c: Colors,
    ) -> Element<'_, Message> {
        let selected = self.settings.palette == palette.name;
        let ground: iced::Color = palette.ground.into();
        let ink: iced::Color = palette.text.into();
        let chip = |rgb: crate::color::Rgb| {
            let fill: iced::Color = rgb.into();
            container(Space::new().width(6).height(6)).style(move |_| c.dot(fill))
        };
        let preview = container(
            column![
                row![
                    text("$")
                        .size(10)
                        .font(Font::MONOSPACE)
                        .color(prompt_ink(palette)),
                    container(Space::new().width(18).height(3))
                        .style(move |_| fill_style(alpha(ink, 0.85), 1.5))
                ]
                .spacing(4)
                .align_y(Center),
                row![
                    chip(palette.ansi[1]),
                    chip(palette.ansi[2]),
                    chip(palette.ansi[3]),
                    chip(palette.ansi[4]),
                    chip(palette.ansi[5])
                ]
                .spacing(3)
            ]
            .spacing(6),
        )
        .padding([7, 8])
        .width(64)
        .height(40)
        .style(move |_| container::Style {
            background: Some(ground.into()),
            border: Border {
                color: alpha(c.text, if c.dark { 0.14 } else { 0.10 }),
                width: 1.0,
                radius: RADIUS_SM.into(),
            },
            ..Default::default()
        });
        let ring = bare(
            format!("palette-{}", palette.name),
            palette.name,
            preview,
            Some(Message::Palette(palette.name.into())),
            2,
            Length::Shrink,
            Length::Shrink,
            move |_, status| ButtonStyle {
                background: None,
                text_color: c.text,
                border: Border {
                    color: if selected {
                        c.accent
                    } else if matches!(status, ButtonStatus::Hovered | ButtonStatus::Pressed) {
                        c.line_strong
                    } else {
                        iced::Color::TRANSPARENT
                    },
                    width: 2.0,
                    radius: (RADIUS_SM + 4.0).into(),
                },
                shadow: Default::default(),
                snap: true,
            },
        );
        column![
            ring,
            text(palette.name)
                .size(12)
                .font(if selected { MEDIUM } else { UI })
                .color(if selected { c.text } else { c.muted })
        ]
        .spacing(6)
        .align_x(Center)
        .into()
    }

    /// An agent's terminal in its frame: who and where over the live grid,
    /// with the state in a word and Detach at the right.
    pub(super) fn terminal_view(&self, c: Colors) -> Element<'_, Message> {
        let Some(terminal) = &self.terminal else {
            return empty(
                "No terminal open",
                "Select a managed session and open its terminal.",
                None,
                c,
            );
        };
        let ended = matches!(terminal.status(), Status::Ended(_));
        let scrolled = terminal.scrolled_back();
        let (tone, word) = if ended {
            (c.faint, "Ended")
        } else if scrolled {
            (c.amber, "Scrolled back")
        } else {
            (c.green, "Live")
        };
        let workdir = self
            .agents
            .iter()
            .find(|agent| agent.id.as_str() == terminal.agent)
            .and_then(|agent| agent.spec.workdir.as_deref())
            .map(tilde);
        let mut who = column![text(self.name_of(&terminal.agent)).size(14).font(MEDIUM)]
            .spacing(1)
            .width(Fill);
        if let Some(workdir) = workdir {
            // One line, cut at the edge rather than pushed under the
            // controls when the folder is deep.
            who = who.push(
                container(
                    text(workdir)
                        .size(11)
                        .font(Font::MONOSPACE)
                        .color(c.faint)
                        .wrapping(iced::widget::text::Wrapping::None),
                )
                .width(Fill)
                .clip(true),
            );
        }
        let mut controls = row![text(word).size(12).font(MEDIUM).color(tone)]
            .spacing(10)
            .align_y(Center);
        if scrolled {
            controls = controls.push(custom(
                "terminal-live",
                "Return to live output",
                text("Return to live output").size(12).font(MEDIUM),
                Some(Message::TerminalScroll(-2000)),
                false,
                Kind::Ghost,
                [4, 8],
            ));
        }
        controls = controls.push(custom(
            "detach-terminal",
            "Detach",
            text("Detach").size(12).font(MEDIUM),
            Some(Message::Detach),
            false,
            Kind::Secondary,
            [4, 10],
        ));
        let mut pane = column![
            container(
                row![dot(tone, 8.0, c), who, controls]
                    .spacing(10)
                    .align_y(Center)
            )
            .padding([10, 14])
        ]
        .width(Fill);
        if let Status::Ended(reason) = terminal.status() {
            pane = pane.push(
                container(text(reason).size(13).color(c.amber))
                    .padding(padding(0.0, 14.0, 10.0, 32.0)),
            );
        }
        if let Some(reason) = terminal.input_notice {
            pane = pane.push(
                container(
                    row![
                        text(reason).size(13).color(c.amber).width(Fill),
                        ghost(
                            "dismiss-terminal-input",
                            "Dismiss",
                            Some(Message::TerminalDismiss),
                        )
                    ]
                    .spacing(8)
                    .align_y(Center),
                )
                .padding(padding(0.0, 14.0, 8.0, 32.0)),
            );
        }
        let palette = self.settings.palette();
        let ground: iced::Color = palette.ground.into();
        pane = pane.push(rule(c)).push(
            container(crate::terminal::Display {
                terminal,
                palette,
                size: self.settings.terminal_size,
                height: (self.shell.height / self.scale_factor() - 336.0).max(240.0),
            })
            .padding(12)
            .width(Fill)
            .style(move |_| fill_style(ground, iced::border::bottom(RADIUS_LG - 1.0))),
        );
        let hint = row![
            keys(&["F6"], c),
            small("leaves terminal input", c),
            text("·").size(12).color(c.faint),
            keys(
                if cfg!(target_os = "macos") {
                    &["⌃", "]"]
                } else {
                    &["Ctrl", "]"]
                },
                c
            ),
            small("detaches", c)
        ]
        .spacing(6)
        .align_y(Center);
        column![frame(pane, c), hint].spacing(10).into()
    }

    /// AgentDocker commands as a command panel: the prompt and the input
    /// over what the commands said, in the terminal palette.
    pub(super) fn console_view(&self, c: Colors) -> Element<'_, Message> {
        let palette = self.settings.palette();
        let ground: iced::Color = palette.ground.into();
        let ink: iced::Color = palette.text.into();
        let quiet = mix(ink, ground, 0.25);
        let prompt = prompt_ink(palette);
        let size = self.settings.terminal_size;
        let mut title_row = row![
            column![
                heading("AgentDocker commands", 16),
                note("Run an AgentDocker command in this project.", c)
            ]
            .spacing(2)
            .width(Fill)
        ]
        .spacing(12)
        .align_y(Center);
        if self.console_running > 0 {
            title_row = title_row.push(status_word("Running", c.accent, c));
        }
        let entry = container(
            row![
                text("$").size(15).font(Font::MONOSPACE).color(c.amber),
                input(
                    "console-command",
                    "A command, e.g. ps, leases or journal",
                    &self.console_input,
                    Message::ConsoleInput
                ),
                ghost(
                    "previous-command",
                    "Previous",
                    (!self.console_history.is_empty()).then_some(Message::Recall(true)),
                ),
                ghost(
                    "next-command",
                    "Next",
                    (!self.console_history.is_empty()).then_some(Message::Recall(false)),
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
        )
        .padding([10, 12]);
        let mut panel = column![entry].width(Fill);
        if self.console_output.is_empty() {
            panel = panel.push(rule(c)).push(
                container(small("Output appears here.", c))
                    .padding([12, 14])
                    .width(Fill),
            );
        } else {
            // Each command line is the prompt in the palette's yellow and
            // the command in its ink; what the command said follows in a
            // quieter ink. Text widgets, not rich text: the output is what
            // assistive technology and the workflow driver read.
            let mut lines = column![].spacing(2);
            let mut block = String::new();
            let mut first = true;
            fn flush<'a>(
                lines: iced::widget::Column<'a, Message>,
                block: &mut String,
                size: f32,
                ink: iced::Color,
            ) -> iced::widget::Column<'a, Message> {
                if block.is_empty() {
                    return lines;
                }
                let words = std::mem::take(block);
                lines.push(
                    text(words.trim_end_matches('\n').to_owned())
                        .size(size)
                        .font(Font::MONOSPACE)
                        .color(ink)
                        .wrapping(iced::widget::text::Wrapping::None),
                )
            }
            for line in self.console_output.split_inclusive('\n') {
                if let Some(command) = line.strip_prefix("$ ") {
                    lines = flush(lines, &mut block, size, quiet);
                    if !first {
                        lines = lines.push(Space::new().height(6));
                    }
                    lines = lines.push(
                        row![
                            text("$").size(size).font(Font::MONOSPACE).color(prompt),
                            text(command.trim_end_matches('\n').to_owned())
                                .size(size)
                                .font(Font::MONOSPACE)
                                .color(ink)
                                .wrapping(iced::widget::text::Wrapping::None)
                        ]
                        .spacing(8),
                    );
                } else {
                    block.push_str(line);
                }
                first = false;
            }
            lines = flush(lines, &mut block, size, quiet);
            panel = panel.push(rule(c)).push(
                container(
                    scrollable(container(lines).padding(padding(12.0, 14.0, 14.0, 14.0)))
                        .direction(iced::widget::scrollable::Direction::Horizontal(
                            iced::widget::scrollable::Scrollbar::new()
                                .width(6)
                                .scroller_width(6)
                                .margin(3),
                        ))
                        .width(Fill)
                        .style(move |theme, status| {
                            // The scrollbar belongs to the terminal ground:
                            // no rail, a thumb in the palette's own ink.
                            let mut style = iced::widget::scrollable::default(theme, status);
                            let dragging = matches!(
                                status,
                                iced::widget::scrollable::Status::Hovered {
                                    is_horizontal_scrollbar_hovered: true,
                                    ..
                                } | iced::widget::scrollable::Status::Dragged {
                                    is_horizontal_scrollbar_dragged: true,
                                    ..
                                }
                            );
                            style.horizontal_rail.background = None;
                            style.horizontal_rail.scroller.background =
                                alpha(ink, if dragging { 0.45 } else { 0.25 }).into();
                            style
                        }),
                )
                .width(Fill)
                .style(move |_| fill_style(ground, iced::border::bottom(RADIUS_LG - 1.0))),
            );
        }
        column![title_row, frame(panel, c)].spacing(14).into()
    }

    /// Installation: whether there is an update, the versions here, what
    /// can be installed from a package, and the plan under review.
    pub(super) fn installation_view(&self, c: Colors) -> Element<'_, Message> {
        let p = &self.desktop;
        let mut screen = column![self.update_banner(c)].spacing(16);
        if p.busy {
            screen = screen.push(notice_line("Verifying installation…", None, c.accent, c));
        }
        if let Some(error) = &p.error {
            screen = screen.push(alert(error.clone(), c));
        }
        if let Some(report) = p
            .report
            .as_ref()
            .filter(|r| r.get("installation").is_none())
        {
            screen = screen.push(self.plan_card(report, c));
        }
        screen = screen.push(self.versions_card(c));
        let narrow = self.narrow();
        let source_input = input(
            "desktop-source",
            "Application bundle or extracted package",
            &p.source,
            Message::DesktopSource,
        );
        let use_current = action(
            "desktop-use-current",
            "Use this application",
            (!p.busy).then_some(Message::DesktopUseCurrent),
            false,
        );
        let source: Element<'_, Message> = if narrow {
            column![source_input, use_current].spacing(8).into()
        } else {
            row![source_input, use_current]
                .spacing(8)
                .align_y(Center)
                .into()
        };
        let mut rows = vec![
            field_row(
                "Package",
                "An application bundle, or a release package you extracted.",
                source,
                c,
            ),
            field_row(
                "Prefix",
                "Where releases are kept. Empty uses your home.",
                input(
                    "desktop-prefix",
                    "Installation prefix (empty uses your home)",
                    &p.prefix,
                    Message::DesktopPrefix,
                ),
                c,
            ),
        ];
        if cfg!(target_os = "macos") {
            rows.push(setting(
                "Preview builds",
                "Allow releases from the preview channel to download and install.",
                switch(
                    "desktop-local",
                    if p.local_preview {
                        "Preview builds allowed"
                    } else {
                        "Allow preview builds"
                    },
                    p.local_preview,
                    (!p.busy).then_some(Message::DesktopLocal(!p.local_preview)),
                    c,
                ),
                c,
            ));
        }
        rows.push(
            container(
                row![
                    Space::new().width(Fill),
                    action(
                        "desktop-install",
                        "Preview installation",
                        p.preview("install")
                            .map(|_| Message::DesktopPreview("install".into())),
                        false,
                    )
                ]
                .align_y(Center),
            )
            .padding([10, 16])
            .into(),
        );
        screen = screen.push(section(
            "Install from a package",
            Some(
                if narrow {
                    "Preview first; activation takes effect on the next launch."
                } else {
                    "Preview an install, rollback, or cleanup before applying it. Activation takes effect on the next app launch; running agents continue."
                }
                .to_owned(),
            ),
            None,
            rows_body(rows, c),
            c,
        ));
        screen.into()
    }

    /// One row says whether there is an update: a disc, the headline, one
    /// sentence, and at most one filled action.
    fn update_banner(&self, c: Colors) -> Element<'_, Message> {
        let p = &self.desktop;
        let update = p
            .update
            .as_ref()
            .or_else(|| p.report.as_ref().and_then(|r| r.get("update")));
        let available = p.update_available();
        let later = available.is_some() && p.later.as_deref() == available;
        let consent = p.preview_consent_needed();
        let apply_pending = p.report.as_ref().is_some_and(|r| r["preview"] == true);
        let installed = update.and_then(|u| {
            u["installed_version"]
                .as_str()
                .or(u["running_version"].as_str())
        });
        let check = |filled: bool| -> Element<'_, Message> {
            let message = p
                .preview("update-check")
                .map(|_| Message::DesktopPreview("update-check".into()));
            if filled {
                primary("desktop-update-check", "Check for updates", message)
            } else {
                action("desktop-update-check", "Check for updates", message, false)
            }
        };
        let (glyph, tone, headline, mut sentence, mut actions) = match available {
            Some(version) => {
                let download = p
                    .preview("update")
                    .map(|_| Message::DesktopPreview("update".into()));
                let label = format!("Download and preview {version}");
                let mut actions = row![].spacing(6).align_y(Center);
                if later || apply_pending {
                    actions = actions.push(check(false)).push(action(
                        "desktop-update",
                        label,
                        download,
                        false,
                    ));
                } else {
                    actions = actions
                        .push(ghost("update-later", "Later", Some(Message::UpdateLater)))
                        .push(primary("desktop-update", label, download));
                }
                (
                    Icon::Download,
                    c.accent,
                    format!("Update available — {version}"),
                    if consent {
                        format!(
                            "{version} is a preview build. Allow preview builds below to download it."
                        )
                    } else if later {
                        "Put off for now. Download and preview it when you are ready.".to_owned()
                    } else {
                        "Downloads and checks it, then shows what would change. Nothing is installed until you apply it.".to_owned()
                    },
                    actions,
                )
            }
            None => {
                let filled = !apply_pending && update.is_none();
                let actions = row![check(filled)].align_y(Center);
                match update {
                    // Nothing on either channel yet: an answer, not a failure.
                    Some(u) if u["published"] == false => (
                        Icon::Check,
                        c.faint,
                        "No update published yet".to_owned(),
                        match installed {
                            Some(version) => format!(
                                "Nothing newer than {version} has been published. Check again later."
                            ),
                            None => "Nothing newer has been published for this installation. Check again later.".to_owned(),
                        },
                        actions,
                    ),
                    Some(u) => (
                        Icon::Check,
                        c.green,
                        "You have the newest release".to_owned(),
                        match installed {
                            Some(version) => {
                                format!("{version} on the {} channel.", field(u, "channel"))
                            }
                            None => format!("On the {} channel.", field(u, "channel")),
                        },
                        actions,
                    ),
                    None => (
                        Icon::Download,
                        c.muted,
                        "Updates".to_owned(),
                        "Checks the published release feed. A check downloads nothing.".to_owned(),
                        actions,
                    ),
                }
            }
        };
        if p.update_check_error {
            sentence = "Couldn’t reach the release feed. Try again.".to_owned();
        }
        if p.checking_updates {
            actions = actions.push(small("Checking…", c));
        }
        let disc = container(icon(glyph, tone, 18.0))
            .center(36)
            .style(move |_| c.dot(alpha(tone, if c.dark { 0.16 } else { 0.12 })));
        let mut words = column![
            text(headline).size(14).font(MEDIUM),
            text(sentence).size(13).color(c.muted)
        ]
        .spacing(2)
        .width(Fill);
        if update.is_some_and(|u| u["state_schema_change"] == true) {
            words = words.push(small(
                "This release changes the daemon's state schema: after installing, rollback needs a matching state backup.",
                c,
            ));
        }
        let content: Element<'_, Message> = if self.narrow() {
            column![
                row![disc, words].spacing(12).align_y(Center),
                row![Space::new().width(Fill), actions.wrap()]
            ]
            .spacing(12)
            .into()
        } else {
            row![disc, words, actions]
                .spacing(14)
                .align_y(Center)
                .into()
        };
        container(content)
            .padding([14, 16])
            .width(Fill)
            .style(move |_| c.card_style())
            .into()
    }

    /// The releases this installation knows of: one available from the
    /// feed, the current one and the one a rollback returns to.
    fn versions_card(&self, c: Colors) -> Element<'_, Message> {
        let p = &self.desktop;
        let update = p
            .update
            .as_ref()
            .or_else(|| p.report.as_ref().and_then(|r| r.get("update")));
        let status = p.report.as_ref().and_then(|r| r.get("installation"));
        let version_row = |version: String,
                           meta: Option<String>,
                           word: &'static str,
                           tone: iced::Color|
         -> Element<'_, Message> {
            let mut words = column![mono(version, 13, c.text)].spacing(2).width(Fill);
            if let Some(meta) = meta {
                words = words.push(mono(meta, 11, c.faint));
            }
            container(
                row![words, status_word(word, tone, c)]
                    .spacing(12)
                    .align_y(Center),
            )
            .padding([10, 16])
            .width(Fill)
            .into()
        };
        let mut rows = Vec::new();
        if let Some(version) = p.update_available() {
            rows.push(version_row(
                version.to_owned(),
                update.map(|u| format!("{} channel", field(u, "channel"))),
                "Available",
                c.accent,
            ));
        }
        match status {
            Some(installation) if installation.is_null() => rows.push(
                container(note("No managed installation at this prefix.", c))
                    .padding([10, 16])
                    .into(),
            ),
            Some(installation) => {
                for (key, word, tone) in [
                    ("current", "Current", c.green),
                    ("previous", "Previous", c.faint),
                ] {
                    if !installation[key].is_null() {
                        rows.push(version_row(
                            field(&installation[key], "version"),
                            Some(format!(
                                "source {}",
                                field(&installation[key], "source_commit")
                            )),
                            word,
                            tone,
                        ));
                    }
                }
            }
            None => match update.and_then(|u| {
                u["installed_version"]
                    .as_str()
                    .or(u["running_version"].as_str())
            }) {
                Some(version) => {
                    rows.push(version_row(version.to_owned(), None, "Current", c.green))
                }
                None => rows.push(
                    container(note(
                        "Show installed versions to read what is installed at this prefix.",
                        c,
                    ))
                    .padding([10, 16])
                    .into(),
                ),
            },
        }
        let mut maintenance = row![].spacing(4).align_y(Center);
        for (operation, label) in [
            ("rollback", "Preview rollback"),
            ("prune", "Preview cleanup"),
            ("uninstall", "Preview removal"),
        ] {
            maintenance = maintenance.push(ghost(
                format!("desktop-{operation}"),
                label,
                p.preview(operation)
                    .map(|_| Message::DesktopPreview(operation.into())),
            ));
        }
        rows.push(container(maintenance.wrap()).padding([6, 10]).into());
        section(
            "Versions",
            None,
            Some(action(
                "desktop-status",
                "Show installed versions",
                p.preview("status")
                    .map(|_| Message::DesktopPreview("status".into())),
                false,
            )),
            rows_body(rows, c),
            c,
        )
    }

    /// The plan an install, update, rollback or cleanup preview made: the
    /// stages it goes through, the facts it pins, and Apply.
    fn plan_card<'a>(&'a self, report: &'a serde_json::Value, c: Colors) -> Element<'a, Message> {
        let p = &self.desktop;
        let preview = report["preview"] == true;
        let mut body = column![].spacing(14).width(Fill);
        if let Some(maintenance) = report.get("maintenance") {
            let mut facts: Vec<(String, Element<'a, Message>)> = Vec::new();
            for path in maintenance["remove"].as_array().into_iter().flatten() {
                facts.push((
                    "Remove".into(),
                    mono(path.as_str().unwrap_or("unknown"), 12, c.text).into(),
                ));
            }
            for entry in maintenance["retained"].as_array().into_iter().flatten() {
                facts.push((
                    "Keep".into(),
                    column![
                        mono(field(entry, "path"), 12, c.text),
                        small(field(entry, "reason"), c)
                    ]
                    .spacing(2)
                    .into(),
                ));
            }
            if facts.is_empty() {
                body = body.push(note("Nothing to remove.", c));
            } else {
                body = body.push(kv_list(facts, c));
            }
        } else {
            // The stages an installation goes through, from what the
            // report proves: a download and a verified package are done
            // by the time there is a preview; Apply installs; the switch
            // to the new release is the daemon's to report.
            let downloaded = report["update"]["downloaded"].is_string();
            let reloaded = report["daemon"]["reloaded"] == true;
            let candidate = &report["candidate"];
            let mut stages: Vec<(Step, &str, Option<String>)> = Vec::new();
            if downloaded {
                stages.push((Step::Done, "Download", None));
            }
            stages.push((
                Step::Done,
                "Verify the package",
                candidate["version"].as_str().map(str::to_owned),
            ));
            stages.push((
                if preview { Step::Next } else { Step::Done },
                if preview {
                    "Install when you apply"
                } else {
                    "Installed"
                },
                None,
            ));
            stages.push((
                if preview {
                    Step::Waiting
                } else if reloaded {
                    Step::Done
                } else {
                    Step::Next
                },
                "Switch over",
                (!reloaded).then(|| "next launch".to_owned()),
            ));
            body = body.push(stage_list(stages, c));
            let mut facts: Vec<(String, Element<'a, Message>)> = vec![(
                "Release".into(),
                column![
                    mono(field(candidate, "version"), 13, c.text),
                    mono(
                        format!("source {}", field(candidate, "source_commit")),
                        11,
                        c.faint
                    )
                ]
                .spacing(2)
                .into(),
            )];
            for (key, label) in [
                ("source", "Package"),
                ("application", "Application"),
                ("bin", "Commands"),
                ("versions", "Versions"),
            ] {
                if let Some(path) = report[key].as_str() {
                    facts.push((
                        label.into(),
                        mono(tilde(std::path::Path::new(path)), 12, c.text).into(),
                    ));
                }
            }
            if let Some(daemon) = report["update"]["daemon"].as_str() {
                facts.push(("Daemon".into(), text(daemon.to_owned()).size(13).into()));
            } else if let Some(summary) = report["daemon"]["summary"].as_str() {
                facts.push(("Daemon".into(), text(summary.to_owned()).size(13).into()));
            }
            body = body.push(kv_list(facts, c));
        }
        if let Some(cleanup) = report.get("extraction_cleanup") {
            body = body.push(note(
                format!(
                    "The operation completed, but temporary update files could not be removed: {} ({})",
                    field(cleanup, "path"),
                    field(cleanup, "error")
                ),
                c,
            ));
        }
        let mut card = column![container(body).padding([14, 16])].width(Fill);
        if preview {
            card = card.push(rule(c)).push(
                container(
                    row![
                        container(status_word("Waiting for you", c.amber, c)).width(Fill),
                        primary(
                            "desktop-apply",
                            "Apply this reviewed plan",
                            p.apply().map(|_| Message::DesktopApply),
                        )
                    ]
                    .spacing(12)
                    .align_y(Center),
                )
                .padding([9, 12])
                .width(Fill)
                .style(move |_| {
                    fill_style(
                        mix(c.card, c.raised, 0.5),
                        iced::border::bottom(RADIUS_LG - 1.0),
                    )
                }),
            );
        }
        let title_text = if preview {
            "Review this plan"
        } else {
            "Installation report"
        };
        frame(
            column![
                container(heading(title_text, 15)).padding([12, 16]),
                rule(c),
                card
            ],
            c,
        )
    }
}

/// The first twelve characters of an id, enough to tell plans apart.
fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

/// One file of a setup plan as a diff: its path and how many entries it
/// gains (or, undone, loses), then each entry as a tinted line with a
/// bar and a sign at its left edge.
fn diff_block<'a>(
    path: String,
    lines: Vec<String>,
    undone: bool,
    c: Colors,
) -> Element<'a, Message> {
    let (sign, tone) = if undone {
        ("−", c.red)
    } else {
        ("+", c.green)
    };
    let count = format!("{sign}{}", lines.len());
    let header = container(
        row![
            icon(Icon::File, c.faint, 13.0),
            mono(path, 12, c.text).width(Fill),
            text(count).size(12).font(Font::MONOSPACE).color(tone)
        ]
        .spacing(8)
        .align_y(Center),
    )
    .padding([7, 10])
    .width(Fill)
    .style(move |_| {
        fill_style(
            mix(c.card, c.raised, 0.55),
            iced::border::top(RADIUS_MD - 1.0),
        )
    });
    let mut block = column![header, rule(c)].width(Fill);
    let count = lines.len();
    for (index, line) in lines.into_iter().enumerate() {
        let last = index + 1 == count;
        let wash = mix(c.card, tone, if c.dark { 0.12 } else { 0.08 });
        let bar = alpha(tone, 0.75);
        // The bar is the outer fill showing through the inner's left
        // padding, so it is always exactly as tall as the line.
        block = block.push(
            container(
                container(
                    row![
                        text(sign)
                            .size(12)
                            .font(Font::MONOSPACE)
                            .color(tone)
                            .width(Length::Fixed(14.0)),
                        mono(line, 12, c.text).width(Fill)
                    ]
                    .spacing(6),
                )
                .padding([4, 10])
                .width(Fill)
                .style(move |_| {
                    fill_style(
                        wash,
                        if last {
                            iced::border::bottom_right(RADIUS_MD - 1.0)
                        } else {
                            0.0.into()
                        },
                    )
                }),
            )
            .padding(padding(0.0, 0.0, 0.0, 3.0))
            .width(Fill)
            .style(move |_| {
                fill_style(
                    mix(c.card, bar, bar.a),
                    if last {
                        iced::border::bottom(RADIUS_MD - 1.0)
                    } else {
                        0.0.into()
                    },
                )
            }),
        );
    }
    container(block)
        .padding(1)
        .width(Fill)
        .style(move |_| container::Style {
            background: Some(c.card.into()),
            border: Border {
                color: c.line,
                width: 1.0,
                radius: RADIUS_MD.into(),
            },
            ..Default::default()
        })
        .into()
}

/// Stages along a rail: a node per stage, tinted by its state, joined by
/// a two-point line, with the stage's words and a quiet fact beside it.
fn stage_list<'a>(stages: Vec<(Step, &str, Option<String>)>, c: Colors) -> Element<'a, Message> {
    let count = stages.len();
    let mut list = column![].width(Fill);
    for (index, (step, label, meta)) in stages.into_iter().enumerate() {
        let tone = match step {
            Step::Done => c.green,
            Step::Next => c.accent,
            Step::Waiting | Step::Undone => c.faint,
        };
        let glyph: Element<'a, Message> = match step {
            Step::Done => icon(Icon::Check, tone, 11.0),
            Step::Next => dot(tone, 6.0, c),
            Step::Waiting | Step::Undone => Space::new().width(0).height(0).into(),
        };
        let node = container(glyph)
            .center(20)
            .style(move |_| container::Style {
                background: Some(
                    if step == Step::Waiting {
                        c.card
                    } else {
                        mix(c.card, tone, if c.dark { 0.20 } else { 0.14 })
                    }
                    .into(),
                ),
                border: Border {
                    color: if step == Step::Waiting {
                        c.line_strong
                    } else {
                        alpha(tone, 0.8)
                    },
                    width: 1.0,
                    radius: 10.0.into(),
                },
                ..Default::default()
            });
        let mut marks = column![node].align_x(Center).width(20);
        if index + 1 < count {
            let joined = if step == Step::Done {
                alpha(c.green, 0.5)
            } else {
                c.line
            };
            marks = marks.push(
                container(Space::new().width(2).height(10)).style(move |_| fill_style(joined, 1.0)),
            );
        }
        let mut words = row![
            text(label.to_owned())
                .size(13)
                .font(if step == Step::Next { MEDIUM } else { UI })
                .color(match step {
                    Step::Done => c.muted,
                    Step::Next => c.text,
                    _ => c.faint,
                })
                .width(Fill)
        ]
        .spacing(10)
        .align_y(Center);
        if let Some(meta) = meta {
            words = words.push(mono(meta, 11, c.faint));
        }
        list = list
            .push(row![marks, container(words).height(20).center_y(20).width(Fill)].spacing(12));
    }
    list.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_mark_reads_as_its_name() {
        assert_eq!(mark_letters("Claude Code"), "CC");
        assert_eq!(mark_letters("Claude Desktop"), "CD");
        assert_eq!(mark_letters("Codex"), "Co");
        assert_eq!(mark_letters("ChatGPT"), "Ch");
        assert_eq!(mark_letters("VS Code (editor)"), "VS");
        assert_eq!(mark_letters(""), "·");
    }

    /// Docker Agent has no file for setup to write: installed, it reads as
    /// wired per agent YAML rather than as an integration that does not
    /// exist, and its details say what AgentDocker can do for its runs.
    #[test]
    fn docker_agent_reads_as_wired_per_agent_yaml_with_its_capabilities() {
        let (tx, _commands) = crate::app::queue::channel();
        let (_sender, rx) = std::sync::mpsc::channel();
        let app = crate::app::App::bare(tx, rx);
        let runtime = RuntimeInfo {
            name: "docker-agent".into(),
            vendor: "Docker".into(),
            label: "Docker Agent".into(),
            cli: Some("/usr/local/lib/docker/cli-plugins/docker-agent".into()),
            version: None,
            apps: vec![],
            extensions: vec![],
            incomplete: vec![],
            config_dir: None,
            mcp: Wiring::Unsupported,
            hooks: Wiring::Unsupported,
            hooks_missing: vec![],
            shell: Wiring::Unsupported,
            running: 0,
        };
        let c = Colors::new(false);
        assert_eq!(
            app.tool_state(&runtime, c).word,
            "Installed · wired in each agent's YAML"
        );
        let spec = agentdocker_core::runtime::spec("docker-agent").unwrap();
        // Builds without panicking for every runtime, installed or not.
        let _ = app.capabilities_list(&runtime, spec, c);
        for spec in agentdocker_core::runtime::RUNTIMES {
            let _ = app.capabilities_list(&runtime, spec, c);
        }
    }

    #[test]
    fn later_puts_off_only_the_version_it_was_pressed_for() {
        let (tx, _commands) = crate::app::queue::channel();
        let (_sender, rx) = std::sync::mpsc::channel();
        let mut app = crate::app::App::bare(tx, rx);
        let _ = app.update(Message::UpdateLater);
        assert_eq!(app.desktop.later, None, "nothing to put off");
        app.desktop.update = Some(serde_json::json!({
            "update_available": true,
            "available": {"version": "0.3.0"}
        }));
        let _ = app.update(Message::UpdateLater);
        assert_eq!(app.desktop.later.as_deref(), Some("0.3.0"));
        // A newer release is a new question: it is not put off.
        app.desktop.update = Some(serde_json::json!({
            "update_available": true,
            "available": {"version": "0.3.1"}
        }));
        assert_ne!(app.desktop.later.as_deref(), app.desktop.update_available());
    }
}
