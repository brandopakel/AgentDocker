//! Questions, approvals, and who is waiting on the person.
//!
//! An agent that needs a decision puts up one of two cards. A question
//! card leads with an icon tile, the question as its title and how long is
//! left; a structured choice follows as full-width option rows, the label
//! on the left and its consequence quietly on the right, a destructive one
//! said in red. Choosing one replaces the rows with the chosen row, filled,
//! as the card's receipt while the answer goes. A Codex approval is a
//! framed card: a strip naming who wants what and the time left, the
//! command (the files, the access) in a darker well, the folder and the
//! reason as detail lines, and a footer whose actions step down in weight —
//! one solid Allow, an outline Review, Deny in plain words.
//!
//! Who is waiting is one card: a header with a dot, a title and a count,
//! then a row per item — who, the ask in one line, the time left and one
//! outline button — with hairlines between. Time left is a small ring
//! drawn on the canvas and kept between frames; nothing here animates.
use super::*;
use crate::accessibility::Semantic;
use crate::controls::Control;
use agentdocker_core::{QuestionOption, QuestionPresentation};
use iced::widget::canvas::{self, Geometry, LineCap, Path, Stroke};
use iced::widget::{Space, column, container, row, scrollable, text};
use iced::{Center, Element, Fill, Font};
use std::cell::Cell;

/// Below this share of its window, time left is said in red.
const URGENT: f32 = 0.2;
/// How finely a ring's arc moves: a 64th of a turn. The sweep that keeps
/// time left honest runs every two seconds; a ring repaints only when its
/// arc visibly moved.
const RING_STEPS: f32 = 64.0;
/// How many waiting items show before "Show N more".
const SHOWN: usize = 3;

/// The tone of time left: amber while there is room, red near the end.
pub(super) fn window_tone(left: f32, c: Colors) -> iced::Color {
    if left < URGENT { c.red } else { c.amber }
}

/// Time left as a ring: a faint track and an arc in the tone, clockwise
/// from twelve o'clock.
struct Ring {
    step: u16,
    tone: iced::Color,
    track: iced::Color,
}

/// A ring's geometry, kept between frames like a glyph's: redrawn only
/// when its step or its inks change.
#[derive(Default)]
struct RingCache {
    geometry: canvas::Cache,
    drawn: Cell<Option<(u16, iced::Color, iced::Color)>>,
}

impl RingCache {
    /// Notes what is about to be drawn; clears the geometry and answers
    /// `true` when it differs from what the cache holds.
    fn refresh(&self, key: (u16, iced::Color, iced::Color)) -> bool {
        if self.drawn.get() == Some(key) {
            return false;
        }
        self.geometry.clear();
        self.drawn.set(Some(key));
        true
    }
}

impl canvas::Program<Message> for Ring {
    type State = RingCache;

    fn draw(
        &self,
        state: &RingCache,
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: iced::Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        state.refresh((self.step, self.tone, self.track));
        vec![state.geometry.draw(renderer, bounds.size(), |frame| {
            let size = bounds.width.min(bounds.height);
            let width = (size / 7.0).clamp(1.5, 2.5);
            let center = frame.center();
            let radius = (size - width) / 2.0;
            frame.stroke(
                &Path::circle(center, radius),
                Stroke::default().with_color(self.track).with_width(width),
            );
            if self.step > 0 {
                let start = -std::f32::consts::FRAC_PI_2;
                let sweep = f32::from(self.step) / RING_STEPS * std::f32::consts::TAU;
                let arc = Path::new(|b| {
                    b.arc(canvas::path::Arc {
                        center,
                        radius,
                        start_angle: iced::Radians(start),
                        end_angle: iced::Radians(start + sweep),
                    });
                });
                frame.stroke(
                    &arc,
                    Stroke::default()
                        .with_color(self.tone)
                        .with_width(width)
                        .with_line_cap(LineCap::Round),
                );
            }
        })]
    }
}

/// How much of something is left, as a ring of `size` points.
pub(super) fn ring<'a>(
    fraction: f32,
    tone: iced::Color,
    size: f32,
    c: Colors,
) -> Element<'a, Message> {
    let step = (fraction.clamp(0.0, 1.0) * RING_STEPS).round() as u16;
    canvas::Canvas::new(Ring {
        step,
        tone,
        track: alpha(c.text, if c.dark { 0.16 } else { 0.13 }),
    })
    .width(size)
    .height(size)
    .into()
}

/// How long is left to answer: a ring and "4m left", or the word Expired.
pub(super) fn time_left<'a>(
    question: &Question,
    now: chrono::DateTime<Utc>,
    c: Colors,
) -> Element<'a, Message> {
    let secs = (question.expires_at - now).num_seconds();
    if secs <= 0 {
        return status_word("Expired", c.faint, c);
    }
    let left = remaining_fraction(question.asked_at, question.expires_at, now);
    row![
        ring(left, window_tone(left, c), 14.0, c),
        text(format!("{} left", super::super::span(secs)))
            .size(12)
            .color(c.muted)
    ]
    .spacing(6)
    .align_y(Center)
    .into()
}

/// Words drawn in two weights and read as one sentence. Text inside a
/// control's bounds is not read on its own, so `sentence` is what an
/// assistive reader (and the workflow driver) finds here, once.
fn spoken<'a>(
    id: String,
    sentence: String,
    content: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    Control {
        content: content.into(),
        semantic: Semantic {
            role: accesskit::Role::Label,
            ..Semantic::button(id, sentence, None)
        },
        button: false,
    }
    .into()
}

/// A labelled action at row height (28 points): rows and card footers,
/// where a 32-point control would crowd the line.
fn compact<'a>(
    id: impl Into<String>,
    label: impl Into<String>,
    message: Option<Message>,
    kind: Kind,
) -> Element<'a, Message> {
    let label = label.into();
    custom(
        id,
        label.clone(),
        text(label)
            .size(13)
            .line_height(iced::Pixels(crate::controls::LABEL_LINE))
            .font(weight(iced::font::Weight::Medium)),
        message,
        false,
        kind,
        [5, if kind == Kind::Primary { 12 } else { 10 }],
    )
}

/// Whether an option undoes or refuses rather than proceeds: said in red.
fn destructive(label: &str) -> bool {
    let first = label
        .split(|ch: char| !ch.is_alphanumeric())
        .find(|word| !word.is_empty())
        .unwrap_or_default()
        .to_lowercase();
    matches!(
        first.as_str(),
        "deny"
            | "cancel"
            | "stop"
            | "reject"
            | "decline"
            | "delete"
            | "discard"
            | "abort"
            | "remove"
            | "revert"
    )
}

/// A consequence short enough to sit at the right of its option's label.
const SIDE_DETAIL: usize = 48;

/// One option of a structured choice: a full-width row with its label on
/// the left and its consequence on the right (beneath it when long). The
/// label is what the row is called; the consequence is its value, so it
/// is read with it.
fn choice<'a>(
    id: String,
    label: String,
    detail: &str,
    message: Option<Message>,
    c: Colors,
) -> Element<'a, Message> {
    let danger = destructive(&label);
    let enabled = message.is_some();
    let detail = detail.trim().to_owned();
    let hint = match (danger, enabled) {
        (true, true) => alpha(c.red, 0.8),
        (true, false) => alpha(c.red, 0.4),
        (false, true) => c.muted,
        (false, false) => alpha(c.muted, 0.55),
    };
    let words = text(label.clone())
        .size(14)
        .line_height(iced::Pixels(20.0))
        .font(weight(iced::font::Weight::Medium));
    let content: Element<'a, Message> = if detail.is_empty() {
        words.width(Fill).into()
    } else if detail.chars().count() <= SIDE_DETAIL {
        row![
            words,
            text(detail.clone())
                .size(12)
                .color(hint)
                .width(Fill)
                .align_x(iced::alignment::Horizontal::Right)
        ]
        .spacing(12)
        .align_y(Center)
        .into()
    } else {
        column![words, text(detail.clone()).size(12).color(hint)]
            .spacing(2)
            .width(Fill)
            .into()
    };
    let button = iced::widget::button(content)
        .padding([8, 12])
        .width(Fill)
        .on_press_maybe(message.clone())
        .style(move |_theme, status| {
            use iced::widget::button::Status;
            let (ink, edge) = if danger {
                (c.red, alpha(c.red, if c.dark { 0.45 } else { 0.35 }))
            } else {
                (c.text, c.line_strong)
            };
            let background = match status {
                Status::Hovered => Some(alpha(ink, if danger { 0.10 } else { 0.05 })),
                Status::Pressed => Some(alpha(ink, if danger { 0.15 } else { 0.08 })),
                Status::Active | Status::Disabled => None,
            };
            let disabled = status == Status::Disabled;
            iced::widget::button::Style {
                background: background.map(Into::into),
                text_color: if disabled { alpha(ink, 0.45) } else { ink },
                border: iced::Border {
                    color: if disabled {
                        alpha(edge, edge.a * 0.5)
                    } else {
                        edge
                    },
                    width: 1.0,
                    radius: super::super::style::RADIUS_SM.into(),
                },
                shadow: Default::default(),
                snap: true,
            }
        });
    let mut semantic = Semantic::button(id, label, message);
    if !detail.is_empty() {
        semantic.value = Some(detail);
    }
    Control {
        content: button.into(),
        semantic,
        button: true,
    }
    .into()
}

/// The chosen option standing in for all of them: filled, with a tick.
/// A refusal is a red wash rather than the solid fill.
fn receipt<'a>(label: String, detail: Option<String>, c: Colors) -> Element<'a, Message> {
    let danger = destructive(&label);
    let (fill, ink) = if danger {
        (alpha(c.red, if c.dark { 0.16 } else { 0.10 }), c.red)
    } else {
        (c.text, c.ground)
    };
    let mut line = row![
        icon(Icon::Check, ink, 14.0),
        text(label)
            .size(14)
            .line_height(iced::Pixels(20.0))
            .font(weight(iced::font::Weight::Medium))
            .color(ink)
    ]
    .spacing(8)
    .align_y(Center);
    if let Some(detail) = detail.filter(|d| !d.trim().is_empty()) {
        line = line.push(
            text(detail)
                .size(12)
                .color(alpha(ink, 0.7))
                .width(Fill)
                .align_x(iced::alignment::Horizontal::Right),
        );
    }
    container(line)
        .padding([8, 12])
        .width(Fill)
        .style(move |_| container::Style {
            background: Some(fill.into()),
            border: iced::Border {
                radius: super::super::style::RADIUS_SM.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

/// A small tile for a card's header: the glyph in amber on an amber wash
/// while the question waits, quiet once it no longer does.
fn ask_tile<'a>(glyph: Icon, waiting: bool, c: Colors) -> Element<'a, Message> {
    let tone = if waiting { c.amber } else { c.faint };
    container(icon(glyph, tone, 16.0))
        .center(32)
        .style(move |_| container::Style {
            background: Some(alpha(tone, if c.dark { 0.14 } else { 0.10 }).into()),
            border: iced::Border {
                color: alpha(tone, 0.28),
                width: 1.0,
                radius: super::super::style::RADIUS_MD.into(),
            },
            ..Default::default()
        })
        .into()
}

/// A question's title and whatever follows it: the first line, and the
/// rest as context.
fn title_and_context(text: &str) -> (String, Option<String>) {
    let text = text.trim();
    match text.split_once('\n') {
        Some((first, rest)) if !rest.trim().is_empty() => {
            (first.trim().to_owned(), Some(rest.trim().to_owned()))
        }
        _ => (text.to_owned(), None),
    }
}

/// What a question asks, in one line for a list: a Codex request by what
/// it would do, a choice by its prompt, anything else by its first line.
fn ask_summary(question: &Question) -> String {
    match question
        .presentation
        .as_ref()
        .filter(|p| p.valid_for(&question.text))
    {
        Some(QuestionPresentation::CodexCommand { command, .. }) => {
            format!("Run: {}", first_line(command, 80))
        }
        Some(QuestionPresentation::CodexFiles { changes, .. }) => match changes.as_slice() {
            [one] => one.label(),
            many => format!("Apply {} file changes", many.len()),
        },
        Some(QuestionPresentation::CodexPermissions { .. }) => {
            "Allow additional access for this turn".to_owned()
        }
        Some(QuestionPresentation::Choices { question, .. }) => compact_question(question),
        None => compact_question(&question.text),
    }
}

/// What a Codex approval asks for, as its card shows it.
enum Request<'q> {
    Command(&'q str),
    Files(&'q [agentdocker_core::QuestionFileChange]),
    Access(&'q agentdocker_core::QuestionPermissions),
}

/// The strip above and below a framed card's body: a step off the card.
fn chrome(c: Colors) -> iced::Color {
    super::super::style::mix(c.card, c.text, if c.dark { 0.03 } else { 0.035 })
}

/// One label and value of a card's detail lines.
fn detail<'a>(label: &'a str, value: Element<'a, Message>, c: Colors) -> Element<'a, Message> {
    row![
        text(label).size(12).color(c.faint).width(56),
        container(value).width(Fill)
    ]
    .spacing(12)
    .into()
}

/// A footer receipt: what was chosen, while it is on its way.
fn receipt_chip<'a>(label: String, c: Colors) -> Element<'a, Message> {
    let danger = destructive(&label);
    let (fill, ink) = if danger {
        (alpha(c.red, if c.dark { 0.16 } else { 0.10 }), c.red)
    } else {
        (c.text, c.ground)
    };
    container(
        row![
            icon(Icon::Check, ink, 12.0),
            text(label)
                .size(12)
                .font(weight(iced::font::Weight::Medium))
                .color(ink)
        ]
        .spacing(6)
        .align_y(Center),
    )
    .padding([5, 10])
    .style(move |_| container::Style {
        background: Some(fill.into()),
        border: iced::Border {
            radius: super::super::style::RADIUS_SM.into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}

/// One row of a waiting list: the words, when, and one action. Narrow,
/// the words stack (who over what) so the ask keeps a line of its own.
fn waiting_row<'a>(
    words: Element<'a, Message>,
    when: Option<Element<'a, Message>>,
    act: Element<'a, Message>,
) -> Element<'a, Message> {
    let mut line = row![container(words).width(Fill)]
        .spacing(14)
        .align_y(Center);
    if let Some(when) = when {
        line = line.push(when);
    }
    container(line.push(act))
        .padding([8, 16])
        .width(Fill)
        .into()
}

/// Who and what, in one line: the subject in the label weight, an aside
/// in the faint ink, the rest in the quiet ink and clipped to the line.
fn waiting_words<'a>(
    subject: String,
    aside: Option<&'a str>,
    predicate: String,
    narrow: bool,
    c: Colors,
) -> Element<'a, Message> {
    let mut who = row![
        text(subject)
            .size(13)
            .font(weight(iced::font::Weight::Medium))
            .wrapping(iced::widget::text::Wrapping::None)
    ]
    .spacing(6)
    .align_y(Center);
    if let Some(aside) = aside {
        who = who.push(text(aside).size(12).color(c.faint));
    }
    let what = container(
        text(predicate)
            .size(13)
            .color(c.muted)
            .wrapping(iced::widget::text::Wrapping::None),
    )
    .width(Fill)
    .clip(true);
    if narrow {
        column![who, what].spacing(2).width(Fill).into()
    } else {
        row![who, what].spacing(10).align_y(Center).into()
    }
}

impl App {
    /// Who is waiting on the person, in one card with one action each:
    /// unanswered questions and paused delivery. Nothing here is new
    /// information; it is the same facts the deeper screens hold, brought
    /// to the first screen so nobody has to know where to look. On a fresh
    /// install, with nothing waiting, it offers the two things that get a
    /// person started instead. Empty when there is nothing to say.
    pub(super) fn needs_you(&self, c: Colors) -> Option<Element<'_, Message>> {
        let now = Utc::now();
        let narrow = self.narrow();
        let mut rows: Vec<Element<'_, Message>> = Vec::new();
        for question in self
            .questions
            .iter()
            .filter(|q| !q.expired(now) && self.agent_on_view(&q.from))
        {
            let name = self.name_of(&question.from);
            // An answer is kept for an ended session, but nobody is
            // reading it now; say so before the person writes.
            let live = self.agent_live(&question.from);
            let what = ask_summary(question);
            let sentence = format!(
                "{name} {}: {what}",
                if live {
                    "asks"
                } else {
                    "asked before its session ended"
                }
            );
            rows.push(waiting_row(
                spoken(
                    format!("needs-you-line-{}", question.id),
                    sentence,
                    waiting_words(name, (!live).then_some("· session ended"), what, narrow, c),
                ),
                Some(time_left(question, now, c)),
                compact(
                    format!("needs-you-answer-{}", question.id),
                    "Answer",
                    Some(Message::OpenQuestion(question.id.clone())),
                    Kind::Secondary,
                ),
            ));
        }
        for agent in self
            .agents
            .iter()
            .filter(|a| self.delivery_needs_you(a) && self.has_project(a.project.as_ref()))
        {
            let name = self.display_name(agent);
            let (joiner, what, control, label) =
                if let Some((_, state)) = agentdocker_core::provider_block(agent, &self.agents) {
                    (
                        ": ",
                        state
                            .issue
                            .as_ref()
                            .expect("blocked")
                            .kind
                            .label()
                            .to_string(),
                        "provider",
                        "Details",
                    )
                } else if agent.status.is_live() {
                    (
                        ": ",
                        "messages are not being delivered".to_owned(),
                        "review",
                        "Review",
                    )
                } else {
                    (
                        " ",
                        format!("ended {}", undelivered_phrase(self.undelivered(agent))),
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
            rows.push(waiting_row(
                spoken(
                    format!("needs-you-line-{}", agent.id),
                    format!("{name}{joiner}{what}"),
                    waiting_words(name, None, what, narrow, c),
                ),
                None,
                compact(
                    format!("needs-you-{control}-{}", agent.id),
                    label,
                    Some(open),
                    Kind::Secondary,
                ),
            ));
        }
        // A question asked in prose, as the opt-in judge read it: worded
        // as the reading it is, and gone once the session works again.
        for agent in self
            .agents
            .iter()
            .filter(|a| self.asked_in_prose(a) && self.has_project(a.project.as_ref()))
        {
            let name = self.display_name(agent);
            let what = "looks to be waiting on you".to_owned();
            rows.push(waiting_row(
                spoken(
                    format!("needs-you-line-{}", agent.id),
                    format!("{name} {what}"),
                    waiting_words(name, None, what, narrow, c),
                ),
                None,
                compact(
                    format!("needs-you-asked-{}", agent.id),
                    "Open",
                    Some(Message::OpenSession(agent.id.to_string())),
                    Kind::Secondary,
                ),
            ));
        }
        // Nobody waiting: on a fresh install the card turns into the two
        // things that get a person started, then disappears for good.
        // Finished sessions are not in it; they are not asking for anything
        // (the Done pill on their row is enough).
        let guidance = rows.is_empty();
        if guidance {
            for process in self.available_processes() {
                // The tool, not a process number: nothing else tells
                // them apart until one is connected and named.
                let tool = super::super::runtime_label(&process.runtime).to_string();
                let connecting = self.shell.adopting.contains(&process.pid);
                rows.push(waiting_row(
                    spoken(
                        format!("needs-you-line-process-{}", process.pid),
                        format!("{tool} is running here, not connected"),
                        waiting_words(
                            tool,
                            None,
                            "is running here, not connected".to_owned(),
                            narrow,
                            c,
                        ),
                    ),
                    None,
                    compact(
                        format!("needs-you-connect-{}", process.pid),
                        if connecting {
                            "Connecting…"
                        } else {
                            "Connect"
                        },
                        (self.connected.is_ok() && !connecting)
                            .then_some(Message::Adopt(process.pid)),
                        Kind::Secondary,
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
                    rows.push(waiting_row(
                        spoken(
                            format!("needs-you-line-tool-{}", runtime.name),
                            format!("{} needs setup", runtime.label),
                            waiting_words(
                                runtime.label.clone(),
                                None,
                                "needs setup".to_owned(),
                                narrow,
                                c,
                            ),
                        ),
                        None,
                        compact(
                            format!("needs-you-tool-{}", runtime.name),
                            "Set up",
                            (!self.setup_busy).then_some(Message::Setup(vec![
                                runtime.name.clone(),
                                "--preview".into(),
                            ])),
                            Kind::Secondary,
                        ),
                    ));
                }
            }
        }
        if rows.is_empty() {
            return None;
        }
        let total = rows.len();
        let mut header = row![
            dot(if guidance { c.cyan } else { c.amber }, 7.0, c),
            text(if guidance {
                "To get started"
            } else {
                "Needs you"
            })
            .size(13)
            .font(weight(iced::font::Weight::Semibold))
        ]
        .spacing(8)
        .align_y(Center);
        if !guidance {
            header = header.push(count_chip(total, true, c));
        }
        let mut list = column![container(header).padding([10, 16]).width(Fill)].width(Fill);
        let shown = if self.shell.needs_you_expanded {
            total
        } else {
            SHOWN
        };
        for item in rows.into_iter().take(shown) {
            list = list.push(rule(c)).push(item);
        }
        if total > SHOWN {
            list = list.push(rule(c)).push(
                container(ghost(
                    "needs-you-more",
                    if self.shell.needs_you_expanded {
                        "Show fewer".to_owned()
                    } else {
                        format!("Show {} more", total - SHOWN)
                    },
                    Some(Message::ToggleNeedsYou),
                ))
                .padding([2, 6]),
            );
        }
        Some(
            container(list)
                .width(Fill)
                .style(move |_| c.card_style())
                .into(),
        )
    }

    /// One question with its controls: a framed approval for a Codex
    /// request, a question card for anything else. The container carries
    /// the notification anchor.
    pub(in crate::app) fn question_card(
        &self,
        question: &Question,
        c: Colors,
    ) -> Element<'_, Message> {
        let now = Utc::now();
        let id = question.id.clone();
        let busy = self.sending.contains(&id);
        let expired = question.expired(now);
        let enabled = !busy && !expired && self.connected.is_ok();
        // The choice on its way, while it is: the card's receipt.
        let chosen = self.shell.chosen_answers.get(&id).filter(|_| busy).cloned();
        let presentation = question
            .presentation
            .as_ref()
            .filter(|p| p.valid_for(&question.text));
        let card = match presentation {
            Some(QuestionPresentation::CodexCommand {
                command,
                cwd,
                reason,
            }) => self.approval_card(
                question,
                Request::Command(command),
                cwd,
                (!reason.trim().is_empty()).then_some(reason.as_str()),
                enabled,
                chosen,
                now,
                c,
            ),
            Some(QuestionPresentation::CodexFiles {
                cwd,
                reason,
                changes,
            }) => self.approval_card(
                question,
                Request::Files(changes),
                cwd,
                (!reason.trim().is_empty() && reason != "Requested by Codex")
                    .then_some(reason.as_str()),
                enabled,
                chosen,
                now,
                c,
            ),
            Some(QuestionPresentation::CodexPermissions {
                cwd,
                reason,
                permissions,
            }) => self.approval_card(
                question,
                Request::Access(permissions),
                cwd,
                (!reason.trim().is_empty() && reason != "Requested by Codex")
                    .then_some(reason.as_str()),
                enabled,
                chosen,
                now,
                c,
            ),
            Some(QuestionPresentation::Choices {
                question: prompt,
                options,
            }) => self.ask_card(question, Some(prompt), options, enabled, chosen, now, c),
            None => self.ask_card(question, None, &[], enabled, None, now, c),
        };
        container(card)
            .id(format!("notification-question-{id}"))
            .into()
    }

    /// A question card: the tile, the question and how long is left; the
    /// options as rows, or the chosen one as the receipt; then a written
    /// answer.
    #[allow(clippy::too_many_arguments)]
    fn ask_card(
        &self,
        question: &Question,
        prompt: Option<&str>,
        options: &[QuestionOption],
        enabled: bool,
        chosen: Option<String>,
        now: chrono::DateTime<Utc>,
        c: Colors,
    ) -> Element<'_, Message> {
        let id = question.id.clone();
        let draft_id = id.clone();
        let busy = self.sending.contains(&id);
        let expired = question.expired(now);
        let (title_text, context) = match prompt {
            Some(prompt) => (prompt.trim().to_owned(), None),
            None => title_and_context(&question.text),
        };
        let mut words = column![
            text(title_text)
                .size(15)
                .line_height(iced::Pixels(22.0))
                .font(weight(iced::font::Weight::Semibold)),
            small(
                format!(
                    "{} · asked {}",
                    self.name_of(&question.from),
                    ago(now, question.asked_at)
                ),
                c
            )
        ]
        .spacing(2)
        .width(Fill);
        if let Some(context) = context {
            words = words.push(container(text(context).size(13).color(c.muted)).padding(
                iced::Padding {
                    top: 6.0,
                    ..Default::default()
                },
            ));
        }
        let state: Element<'_, Message> = if busy {
            status_word("Sending", c.muted, c)
        } else {
            self.answer_window(question, c)
        };
        let mut body = column![
            row![ask_tile(Icon::Question, !expired, c), words, state]
                .spacing(12)
                .align_y(iced::Alignment::Start)
        ]
        .spacing(14);
        let resolved = chosen.and_then(|value| options.iter().find(|o| o.label == value));
        if let Some(option) = resolved {
            body = body.push(receipt(
                option.label.clone(),
                Some(option.description.clone()),
                c,
            ));
        } else {
            if !options.is_empty() {
                let mut list = column![].spacing(6).width(Fill);
                for (index, option) in options.iter().enumerate() {
                    list = list.push(choice(
                        format!("answer-choice-{id}-{index}"),
                        option.label.clone(),
                        &option.description,
                        enabled.then(|| Message::AnswerChoice(id.clone(), option.label.clone())),
                        c,
                    ));
                }
                body = body.push(list);
            }
            let answer = self.shell.answers.get(&id).cloned().unwrap_or_default();
            let send =
                (enabled && !answer.trim().is_empty()).then_some(Message::Answer(id.clone()));
            let label = if busy { "Sending…" } else { "Send answer" };
            // With options to choose from, writing is the second way to
            // answer and its button steps down to an outline.
            let send_button = if options.is_empty() {
                primary(format!("send-answer-{id}"), label, send.clone())
            } else {
                action(format!("send-answer-{id}"), label, send.clone(), false)
            };
            body = body.push(
                row![
                    composer(
                        format!("answer-{id}"),
                        id.to_string(),
                        if options.is_empty() {
                            "Your answer"
                        } else {
                            "Or write an answer"
                        },
                        &answer,
                        move |text| Message::Draft(draft_id.clone(), text),
                        enabled,
                        send,
                    ),
                    send_button
                ]
                .spacing(8)
                .align_y(Center),
            );
        }
        if let Some(error) = self.shell.answer_errors.get(&id) {
            body = body.push(notice_line("Not sent", Some(error.clone()), c.amber, c));
        }
        container(body)
            .padding(16)
            .width(Fill)
            .style(move |_| c.card_style())
            .into()
    }

    /// A Codex request as a framed card: who wants what and the time left
    /// in a strip, the request in a darker well, the folder and reason as
    /// detail lines, and the footer's actions in three weights.
    #[allow(clippy::too_many_arguments)]
    fn approval_card(
        &self,
        question: &Question,
        request: Request<'_>,
        cwd: &str,
        reason: Option<&str>,
        enabled: bool,
        chosen: Option<String>,
        now: chrono::DateTime<Utc>,
        c: Colors,
    ) -> Element<'_, Message> {
        let id = question.id.clone();
        let busy = self.sending.contains(&id);
        let expired = question.expired(now);
        let review = self.shell.file_review.as_ref() == Some(&id);
        let (glyph, wants) = match request {
            Request::Command(_) => (Icon::Terminal, "wants to run"),
            Request::Files(_) => (Icon::File, "wants to change files"),
            Request::Access(_) => (Icon::Shield, "asks for more access"),
        };
        let state: Element<'_, Message> = if busy {
            status_word("Sending", c.muted, c)
        } else {
            self.answer_window(question, c)
        };
        let strip = container(
            row![
                icon(glyph, c.muted, 14.0),
                row![
                    text(self.name_of(&question.from))
                        .size(12)
                        .font(weight(iced::font::Weight::Medium))
                        .color(c.text),
                    text(wants).size(12).color(c.muted)
                ]
                .spacing(4)
                .width(Fill),
                state
            ]
            .spacing(8)
            .align_y(Center),
        )
        .padding([6, 12])
        .height(30)
        .align_y(Center)
        .width(Fill)
        .style(move |_| container::Style {
            background: Some(chrome(c).into()),
            border: iced::Border {
                radius: iced::border::top(super::super::style::RADIUS_LG - 1.0),
                ..Default::default()
            },
            ..Default::default()
        });

        let code = |value: String, ink: iced::Color| {
            text(value)
                .size(12)
                .line_height(iced::Pixels(18.0))
                .font(Font::MONOSPACE)
                .color(ink)
                .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
        };
        let well: Element<'_, Message> = match request {
            Request::Command(command) => row![
                text("$")
                    .size(13)
                    .line_height(iced::Pixels(20.0))
                    .font(Font::MONOSPACE)
                    .color(c.amber),
                text(command.to_owned())
                    .size(13)
                    .line_height(iced::Pixels(20.0))
                    .font(Font::MONOSPACE)
                    .color(c.text)
                    .width(Fill)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                custom_sized(
                    format!("copy-command-{id}"),
                    "Copy command",
                    container(icon(Icon::Copy, c.muted, 14.0)).center(20),
                    Some(Message::CopyGuidance(command.to_owned())),
                    false,
                    Kind::Ghost,
                    [0, 0],
                    iced::Length::Shrink,
                )
            ]
            .spacing(8)
            .align_y(iced::Alignment::Start)
            .into(),
            Request::Files(changes) if review => {
                let mut diffs = column![].spacing(12).width(Fill);
                for change in changes {
                    diffs = diffs.push(
                        column![
                            code(change.label(), c.muted),
                            code(change.diff.clone(), c.text)
                        ]
                        .spacing(4),
                    );
                }
                container(scrollable(diffs).height(iced::Shrink))
                    .max_height(320)
                    .into()
            }
            Request::Files(changes) => {
                let mut list = column![].spacing(2).width(Fill);
                for change in changes {
                    list = list.push(code(change.label(), c.text));
                }
                list.into()
            }
            Request::Access(permissions) => {
                let mut list = column![].spacing(2).width(Fill);
                for line in permissions.lines() {
                    list = list.push(code(line, c.text));
                }
                list.into()
            }
        };
        let well = container(well)
            .padding([10, 12])
            .width(Fill)
            .style(move |_| container::Style {
                background: Some(if c.dark { c.ground } else { c.card }.into()),
                ..Default::default()
            });

        let mut details = column![detail(
            "Folder",
            mono(cwd.to_owned(), c)
                .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                .into(),
            c
        )]
        .spacing(6)
        .width(Fill);
        if let Some(reason) = reason {
            details = details.push(detail(
                "Reason",
                text(reason.to_owned()).size(13).color(c.text).into(),
                c,
            ));
        }

        let left: Element<'_, Message> = if expired {
            small("No longer waiting for an answer", c).into()
        } else {
            text(format!("Asked {}", ago(now, question.asked_at)))
                .size(12)
                .color(c.faint)
                .into()
        };
        let mut actions = row![].spacing(6).align_y(Center);
        if let Some(chosen) = chosen {
            actions = actions.push(receipt_chip(chosen, c));
        } else {
            actions = actions.push(compact(
                format!("answer-deny-{id}"),
                "Deny",
                enabled.then(|| Message::AnswerChoice(id.clone(), "Deny".into())),
                Kind::Ghost,
            ));
            if matches!(request, Request::Files(_)) {
                actions = actions.push(compact(
                    format!("review-files-{id}"),
                    if review {
                        "Hide changes"
                    } else {
                        "Review changes"
                    },
                    Some(Message::ReviewFiles(id.clone())),
                    Kind::Secondary,
                ));
            }
            // Files are allowed only once their changes have been looked at.
            let allow = enabled && (review || !matches!(request, Request::Files(_)));
            actions = actions.push(compact(
                format!("answer-allow-{id}"),
                match (busy, &request) {
                    (true, _) => "Sending…",
                    (false, Request::Access(_)) => "Allow for this turn",
                    (false, _) => "Allow once",
                },
                allow.then(|| Message::AnswerChoice(id.clone(), "Allow".into())),
                Kind::Primary,
            ));
        }
        let footer = container(
            row![container(left).width(Fill), actions]
                .spacing(12)
                .align_y(Center),
        )
        .padding(iced::Padding {
            top: 6.0,
            right: 8.0,
            bottom: 6.0,
            left: 12.0,
        })
        .width(Fill)
        .style(move |_| container::Style {
            background: Some(chrome(c).into()),
            border: iced::Border {
                radius: iced::border::bottom(super::super::style::RADIUS_LG - 1.0),
                ..Default::default()
            },
            ..Default::default()
        });

        let mut frame = column![
            strip,
            rule(c),
            well,
            rule(c),
            container(details).padding([10, 12]).width(Fill)
        ]
        .width(Fill);
        if let Some(error) = self.shell.answer_errors.get(&id) {
            frame = frame.push(
                container(notice_line("Not sent", Some(error.clone()), c.amber, c))
                    .padding([0, 12])
                    .width(Fill),
            );
            frame = frame.push(Space::new().height(10));
        }
        frame = frame.push(rule(c)).push(footer);
        container(frame)
            .padding(1)
            .width(Fill)
            .style(move |_| c.card_style())
            .into()
    }

    /// How long is left to answer, as a ring and a few words. An expired
    /// question says so instead.
    pub(super) fn answer_window(&self, question: &Question, c: Colors) -> Element<'_, Message> {
        time_left(question, Utc::now(), c)
    }

    /// Inbox as a messenger, for a daemon that keeps no conversations: a
    /// card of who is talking to you on the left, the chosen conversation
    /// on the right with its questions first, because they carry controls
    /// a message cannot, and a composer under it.
    pub(super) fn questions(&self, c: Colors) -> Element<'_, Message> {
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

        // Left: who is talking to you, in one card.
        let total_waiting: usize = threads.iter().map(|t| t.2).sum();
        let mut header = row![
            icon(Icon::Inbox, c.faint, 15.0),
            text("Conversations")
                .size(15)
                .font(weight(iced::font::Weight::Semibold))
        ]
        .spacing(8)
        .align_y(Center);
        if total_waiting > 0 {
            header = header.push(count_chip(total_waiting, true, c));
        }
        let mut list = column![].spacing(2).width(Fill);
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
            everyone = everyone.push(count_chip(total_waiting, true, c));
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
                        text(ago(now, *at)).size(12).color(c.faint)
                    ]
                    .spacing(6)
                    .align_y(Center),
                    container(
                        text(preview)
                            .size(12)
                            .color(c.muted)
                            .wrapping(iced::widget::text::Wrapping::None)
                    )
                    .width(Fill)
                    .clip(true)
                ]
                .spacing(1)
                .width(Fill)
            ]
            .spacing(10)
            .align_y(Center);
            if *waiting > 0 {
                row_content = row_content.push(count_chip(*waiting, true, c));
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
        let people = container(column![
            container(header).padding([12, 16]).width(Fill),
            rule(c),
            container(list).padding(4).width(Fill)
        ])
        .width(Fill)
        .style(move |_| c.card_style());

        // Right: the conversation.
        let mut convo = column![].spacing(12).width(Fill);
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
                compact(
                    format!("dismiss-shown-{}", ids[0]),
                    "Clear shown",
                    enabled.then_some(Message::DismissInbox(ids.clone())),
                    Kind::Ghost,
                )
            ]);
        }
        let mut messages = column![].spacing(2).width(Fill);
        let mut last_from: Option<String> = None;
        for message in shown {
            let from = self.canonical_agent(&message.from).to_owned();
            let show_name = last_from.as_deref() != Some(from.as_str()) || selected.is_none();
            last_from = Some(from.clone());
            messages = messages.push(self.bubble(message, show_name, c));
        }
        convo = convo.push(messages);
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
                composer = composer.push(notice_line("Not sent", Some(error.clone()), c.amber, c));
            } else if entry.is_some_and(|e| e.queued.is_some()) && !sending {
                composer = composer.push(small("Queued for the agent.", c));
            } else {
                composer = composer.push(small(
                    "Send goes to this agent. Send to everyone reaches every agent in its project.",
                    c,
                ));
            }
            if let Some(notice) = entry.and_then(|entry| {
                super::super::send_readiness::notice(
                    &entry.draft,
                    super::super::shell::DeliveryTarget::Session(id.to_owned()),
                    c,
                )
            }) {
                composer = composer.push(notice);
            }
            convo = convo.push(container(composer).padding(iced::Padding {
                top: 4.0,
                ..Default::default()
            }));
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
                    row![
                        icon(Icon::ChevronLeft, c.muted, 14.0),
                        text("Conversations")
                            .size(13)
                            .font(weight(iced::font::Weight::Medium))
                    ]
                    .spacing(4)
                    .align_y(Center),
                    Some(Message::InboxList),
                    false,
                    Kind::Ghost,
                    [5, 8],
                );
                column![back, convo].spacing(12).into()
            } else {
                column![people].spacing(14).into()
            }
        } else {
            row![container(people).width(300), container(convo).width(Fill)]
                .spacing(16)
                .into()
        }
    }

    /// One message of a conversation: who and when on top, the text
    /// beneath, long texts folded until asked for. A run of messages from
    /// one sender shows the name and mark once.
    pub(super) fn bubble(
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
        let name = self.name_of(&message.from);
        let mut head = row![].spacing(8).align_y(Center);
        if show_name {
            head = head.push(
                text(name.clone())
                    .size(13)
                    .font(weight(iced::font::Weight::Semibold)),
            );
        }
        if message.kind != "chat" && message.kind != "message" {
            head = head.push(text(message.kind.to_string()).size(12).color(c.faint));
        }
        head = head.push(
            text(ago(Utc::now(), message.sent_at))
                .size(12)
                .color(c.faint),
        );
        head = head.push(Space::new().width(Fill));
        if self.inbox.iter().any(|m| m.id == id) {
            let busy = self.dismissing.contains(&id);
            head = head.push(compact(
                format!("dismiss-message-{id}"),
                if busy { "…" } else { "Clear" },
                (!busy && self.connected.is_ok())
                    .then_some(Message::DismissInbox(vec![id.clone()])),
                Kind::Ghost,
            ));
        }
        let mut body = column![head, text(shown_text).size(14)]
            .spacing(2)
            .max_width(760);
        if long {
            body = body.push(compact(
                format!("message-detail-{id}"),
                match (question, expanded) {
                    (true, true) => "Hide question",
                    (true, false) => "Show question",
                    (false, true) => "Show less",
                    (false, false) => "Show more",
                },
                Some(Message::QuestionDetails(id.clone())),
                Kind::Ghost,
            ));
        }
        let mark: Element<'_, Message> = if show_name {
            monogram(&name, &message.from, 28.0, c)
        } else {
            Space::new().width(28).into()
        };
        container(row![mark, body].spacing(10).align_y(iced::Alignment::Start))
            .padding([6, 4])
            .width(Fill)
            .id(format!("notification-message-{id}"))
            .into()
    }

    /// Messages as a transcript: when, who, what — one line each, the way
    /// a chat client reads, rather than a card per message. Rows sit in
    /// one hairline frame, separated by rules.
    pub(super) fn transcript<'a>(
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
        let mut lines = column![].spacing(0).width(Fill);
        if dismissible.len() > 1 {
            let enabled = self.connected.is_ok()
                && dismissible.iter().all(|id| !self.dismissing.contains(id));
            lines = lines
                .push(
                    container(row![
                        Space::new().width(Fill),
                        compact(
                            format!("dismiss-shown-{}", dismissible[0]),
                            "Dismiss shown",
                            enabled.then_some(Message::DismissInbox(dismissible.clone())),
                            Kind::Ghost,
                        )
                    ])
                    .padding([2, 6]),
                )
                .push(rule(c));
        }
        let mut first = true;
        for message in messages {
            if !first {
                lines = lines.push(rule(c));
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
                who = who.push(
                    text(if earlier_question {
                        "Earlier question".to_owned()
                    } else {
                        message.kind.to_string()
                    })
                    .size(12)
                    .color(c.faint),
                );
            }
            who = who.push(Space::new().width(Fill));
            if self.inbox.iter().any(|item| item.id == message.id)
                && !self
                    .questions
                    .iter()
                    .any(|question| question.id == message.id)
            {
                let busy = self.dismissing.contains(&message.id);
                who = who.push(compact(
                    format!("dismiss-message-{}", message.id),
                    if busy { "Dismissing…" } else { "Dismiss" },
                    (!busy && self.connected.is_ok())
                        .then_some(Message::DismissInbox(vec![message.id.clone()])),
                    Kind::Ghost,
                ));
            }
            let mut content = column![who].spacing(2).width(Fill);
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
                content = content.push(compact(
                    format!("message-detail-{}", message.id),
                    if expanded {
                        "Hide question"
                    } else {
                        "Show question"
                    },
                    Some(Message::QuestionDetails(message.id.clone())),
                    Kind::Ghost,
                ));
            } else {
                content = content.push(text(payload).size(13));
            }
            lines = lines.push(
                container(
                    row![
                        container(
                            text(ago(Utc::now(), message.sent_at))
                                .size(12)
                                .color(c.faint)
                        )
                        .width(64)
                        .padding(iced::Padding {
                            top: 5.0,
                            ..Default::default()
                        }),
                        content
                    ]
                    .spacing(10),
                )
                .padding([6, 12])
                .width(Fill)
                .id(format!("notification-message-{}", message.id)),
            );
        }
        container(lines)
            .width(Fill)
            .style(move |_| container::Style {
                border: iced::Border {
                    color: c.line,
                    width: 1.0,
                    radius: super::super::style::RADIUS_MD.into(),
                },
                ..Default::default()
            })
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::widget::{Operation, Tree, operation::Outcome};
    use iced::advanced::{Layout, layout};

    /// What a rendered element offers: its controls (id, label, value,
    /// enabled), its readable texts and its anchored containers.
    #[derive(Default)]
    struct Seen {
        controls: Vec<(String, String, Option<String>, bool)>,
        texts: Vec<String>,
        anchors: Vec<iced::advanced::widget::Id>,
    }
    impl Seen {
        fn control(&self, id: &str) -> Option<&(String, String, Option<String>, bool)> {
            self.controls.iter().find(|c| c.0 == id)
        }
        fn reads(&self, words: &str) -> bool {
            self.texts.iter().any(|t| t.contains(words))
                || self.controls.iter().any(|c| c.1.contains(words))
        }
    }
    impl Operation for Seen {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn container(&mut self, id: Option<&iced::advanced::widget::Id>, _: iced::Rectangle) {
            if let Some(id) = id {
                self.anchors.push(id.clone());
            }
        }
        fn text(&mut self, _: Option<&iced::advanced::widget::Id>, _: iced::Rectangle, text: &str) {
            self.texts.push(text.to_owned());
        }
        fn custom(
            &mut self,
            _: Option<&iced::advanced::widget::Id>,
            _: iced::Rectangle,
            state: &mut dyn std::any::Any,
        ) {
            if let Some(semantic) = state.downcast_ref::<Semantic>() {
                self.controls.push((
                    semantic.id.clone(),
                    semantic.label.clone(),
                    semantic.value.clone(),
                    semantic.action.is_some() || semantic.change.is_some(),
                ));
            }
        }
        fn finish(&self) -> Outcome<()> {
            Outcome::None
        }
    }

    fn seen(mut element: Element<'_, Message>) -> Seen {
        let renderer = iced::Renderer::new(iced::Font::DEFAULT, 14.0.into());
        let mut tree = Tree::new(&element);
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(iced::Size::ZERO, iced::Size::new(900.0, 6000.0)),
        );
        let mut seen = Seen::default();
        element
            .as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut seen);
        seen
    }

    /// A window with a connected daemon. The command queue's receiver is
    /// returned so sends stay queued (a dropped one refuses them).
    fn app() -> (App, super::super::super::queue::Receiver) {
        let (tx, commands) = super::super::super::queue::channel();
        let (_, rx) = std::sync::mpsc::sync_channel(super::super::super::MESSAGE_CAPACITY);
        let mut app = App::bare(tx, rx);
        app.connected = Ok(());
        (app, commands)
    }

    fn asked(app: &mut App, id: &str, presentation: QuestionPresentation) -> Question {
        let question = Question {
            id: agentdocker_core::MessageId::from(id.to_owned()),
            from: "asker".into(),
            to: agentdocker_core::Destination::Agent("human".into()),
            text: presentation.text(),
            presentation: Some(presentation),
            asked_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(10),
        };
        app.questions.push(question.clone());
        question
    }

    /// Each option is one control named by its label with its consequence
    /// as its value; the written answer stays beside them; choosing one
    /// leaves only the chosen row, as the receipt, until the answer lands.
    #[test]
    fn a_choice_becomes_the_cards_receipt_while_its_answer_goes() {
        let (mut app, _commands) = app();
        let question = asked(
            &mut app,
            "choice",
            QuestionPresentation::Choices {
                question: "Which fixture route?".into(),
                options: vec![
                    QuestionOption {
                        label: "Blue".into(),
                        description: "Use the blue route".into(),
                    },
                    QuestionOption {
                        label: "Cancel".into(),
                        description: String::new(),
                    },
                ],
            },
        );
        let c = Colors::new(false);
        let before = seen(app.question_card(&question, c));
        let blue = before
            .control("answer-choice-choice-0")
            .expect("first option");
        assert_eq!(
            (blue.1.as_str(), blue.2.as_deref(), blue.3),
            ("Blue", Some("Use the blue route"), true)
        );
        let cancel = before
            .control("answer-choice-choice-1")
            .expect("second option");
        assert_eq!((cancel.1.as_str(), cancel.2.as_deref()), ("Cancel", None));
        assert!(
            before.control("answer-choice").is_some(),
            "the written answer"
        );
        assert!(before.control("send-answer-choice").is_some());
        assert!(before.reads("Which fixture route?"));
        assert!(
            before.anchors.contains(&iced::advanced::widget::Id::from(
                "notification-question-choice"
            )),
            "the notification anchor stays on the card"
        );

        let _ = app.update(Message::AnswerChoice(question.id.clone(), "Cancel".into()));
        assert!(app.sending.contains(&question.id));
        let during = seen(app.question_card(&question, c));
        assert!(during.control("answer-choice-choice-0").is_none());
        assert!(during.control("answer-choice-choice-1").is_none());
        assert!(
            during.control("answer-choice").is_none(),
            "no second answer while one goes"
        );
        assert!(during.reads("Cancel"), "the receipt names the choice");
        assert!(during.reads("Sending"));
    }

    /// A Codex command keeps its two answers and their ids, offers no
    /// written answer, and its reason stays readable text.
    #[test]
    fn a_command_approval_keeps_its_answers_and_reads_its_reason() {
        let (mut app, _commands) = app();
        let question = asked(
            &mut app,
            "command",
            QuestionPresentation::CodexCommand {
                command: "printf fixture".into(),
                cwd: "/owned".into(),
                reason:
                    "Requested connection: example.com (https)\n\nDeny cancels this Codex request."
                        .into(),
            },
        );
        for dark in [false, true] {
            let view = seen(app.question_card(&question, Colors::new(dark)));
            let allow = view.control("answer-allow-command").expect("allow");
            assert_eq!((allow.1.as_str(), allow.3), ("Allow once", true));
            let deny = view.control("answer-deny-command").expect("deny");
            assert_eq!((deny.1.as_str(), deny.3), ("Deny", true));
            assert!(
                view.control("answer-command").is_none(),
                "no written answer"
            );
            assert!(view.reads("printf fixture"));
            assert!(view.reads("Requested connection: example.com (https"));
            assert!(view.reads("Deny cancels this Codex request."));
            assert!(view.reads("/owned"));
        }
        let _ = app.update(Message::AnswerChoice(question.id.clone(), "Allow".into()));
        let during = seen(app.question_card(&question, Colors::new(true)));
        assert!(during.control("answer-allow-command").is_none());
        assert!(during.control("answer-deny-command").is_none());
        assert!(during.reads("Allow"), "the footer says what was chosen");
    }

    /// A written answer is not a choice: a choice made earlier for the same
    /// question is forgotten, so the card never shows the wrong receipt.
    #[test]
    fn a_written_answer_forgets_an_earlier_choice() {
        let (mut app, _commands) = app();
        let question = asked(
            &mut app,
            "written",
            QuestionPresentation::Choices {
                question: "Which route?".into(),
                options: vec![QuestionOption {
                    label: "Blue".into(),
                    description: String::new(),
                }],
            },
        );
        let _ = app.update(Message::AnswerChoice(question.id.clone(), "Blue".into()));
        assert_eq!(
            app.shell
                .chosen_answers
                .get(&question.id)
                .map(String::as_str),
            Some("Blue")
        );
        app.sending.remove(&question.id);
        app.shell
            .answers
            .insert(question.id.clone(), "Green, please".into());
        let _ = app.update(Message::Answer(question.id.clone()));
        assert!(app.sending.contains(&question.id));
        assert!(!app.shell.chosen_answers.contains_key(&question.id));
        let during = seen(app.question_card(&question, Colors::new(false)));
        assert!(
            during.control("answer-written").is_some(),
            "the written answer stays"
        );
    }

    /// Needs you reads each row as one sentence and keeps its action's id.
    #[test]
    fn needs_you_reads_each_row_as_one_sentence() {
        let (mut app, _commands) = app();
        let mut agent = AgentRecord::new(
            agentdocker_core::AgentSpec {
                name: "terminal-fixture".into(),
                runtime: "codex".into(),
                ..Default::default()
            },
            false,
            Utc::now(),
        );
        agent.status = agentdocker_core::AgentStatus::Running;
        let id = agent.id.to_string();
        app.agents.push(agent);
        let presentation = QuestionPresentation::Choices {
            question: "Which fixture route?".into(),
            options: vec![QuestionOption {
                label: "Blue".into(),
                description: String::new(),
            }],
        };
        app.questions.push(Question {
            id: agentdocker_core::MessageId::from("ask".to_owned()),
            from: id,
            to: agentdocker_core::Destination::Agent("human".into()),
            text: presentation.text(),
            presentation: Some(presentation),
            asked_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(10),
        });
        let view = seen(
            app.needs_you(Colors::new(true))
                .expect("a waiting question"),
        );
        let line = view
            .control("needs-you-line-ask")
            .expect("the row's sentence");
        assert_eq!(line.1, "terminal-fixture asks: Which fixture route?");
        let answer = view
            .control("needs-you-answer-ask")
            .expect("the row's action");
        assert_eq!((answer.1.as_str(), answer.3), ("Answer", true));
        assert!(view.reads("Needs you"));
        assert!(view.reads("left"), "time left beside the ask");
    }

    #[test]
    fn a_ring_is_redrawn_only_when_its_step_or_its_inks_change() {
        let cache = RingCache::default();
        let (amber, red, track) = (
            iced::Color::from_rgb(0.9, 0.7, 0.3),
            iced::Color::from_rgb(0.9, 0.4, 0.4),
            iced::Color::from_rgba(1.0, 1.0, 1.0, 0.16),
        );
        assert!(cache.refresh((40, amber, track)), "first draw");
        assert!(!cache.refresh((40, amber, track)), "nothing moved");
        assert!(cache.refresh((39, amber, track)), "the arc moved a step");
        assert!(cache.refresh((39, red, track)), "the tone changed");
        assert!(!cache.refresh((39, red, track)));
    }

    #[test]
    fn time_left_turns_red_in_the_last_fifth() {
        let c = Colors::new(true);
        assert_eq!(window_tone(0.5, c), c.amber);
        assert_eq!(window_tone(0.2, c), c.amber);
        assert_eq!(window_tone(0.19, c), c.red);
    }

    #[test]
    fn refusals_are_told_apart_from_choices_that_proceed() {
        for label in [
            "Deny",
            "Cancel",
            "Stop the run",
            "reject",
            "Delete everything",
        ] {
            assert!(destructive(label), "{label}");
        }
        for label in ["Allow", "Blue", "Run it", "Dry run first", "Stopwatch", ""] {
            assert!(!destructive(label), "{label}");
        }
    }

    #[test]
    fn a_question_reads_its_first_line_as_the_title_and_the_rest_as_context() {
        assert_eq!(
            title_and_context("Use the fixture API?"),
            ("Use the fixture API?".to_owned(), None)
        );
        assert_eq!(
            title_and_context("Which route?\n\nBlue is faster.\n"),
            (
                "Which route?".to_owned(),
                Some("Blue is faster.".to_owned())
            )
        );
    }
}
