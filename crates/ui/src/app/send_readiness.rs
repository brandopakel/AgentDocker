//! Compact send-time warnings belong to the draft destination they answered.
use super::{
    Message,
    icons::{Icon, icon},
    messages::look::{link, status_line},
    shell::{DeliveryTarget, TextDraft},
    style::{Colors, RADIUS_MD, alpha, mix, weight},
    view::{small, tip},
};
use crate::controls::{Kind, button as action, custom_sized};
use iced::{
    Element, Fill, Font,
    widget::{column, container, row, scrollable, text},
};

pub(super) fn notice(
    draft: &TextDraft,
    target: DeliveryTarget,
    c: Colors,
) -> Option<Element<'static, Message>> {
    notice_content(draft, target, c, true)
}

/// A composer already scrolls all its feedback within the available pane.
pub(super) fn composer_notice(
    draft: &TextDraft,
    target: DeliveryTarget,
    c: Colors,
) -> Option<Element<'static, Message>> {
    notice_content(draft, target, c, false)
}

/// One status line — an amber dot, `Queued · 1 session needs attention`
/// and a quiet Details link — and, opened, who needs what and the two
/// things to do about each, in a quiet box under it.
fn notice_content(
    draft: &TextDraft,
    target: DeliveryTarget,
    c: Colors,
    scroll_details: bool,
) -> Option<Element<'static, Message>> {
    let report = draft.readiness.as_ref()?;
    if report.needs_attention == 0 {
        return None;
    }
    let key = match &target {
        DeliveryTarget::Conversation(key) => format!("conversation-{key}"),
        DeliveryTarget::Session(key) => format!("session-{key}"),
    };
    let expanded = draft.readiness_expanded;
    let toggle = link(
        format!("delivery-details-{key}"),
        if expanded {
            "Hide details"
        } else {
            "Delivery details"
        },
        if expanded { "Hide details" } else { "Details" },
        Some(Message::DeliveryDetails(target)),
        c.accent,
    );
    let mut body = column![status_line(
        c.amber,
        format!(
            "Queued · {} {}",
            report.needs_attention,
            if report.needs_attention == 1 {
                "session needs attention"
            } else {
                "sessions need attention"
            }
        ),
        c.amber,
        vec![toggle],
        c,
    )]
    .spacing(6);
    if expanded {
        // Who, what, and the way to the fix: the session's panel holds the
        // command or the button that does it.
        let mut details = column![].spacing(8);
        for recipient in &report.details {
            details = details.push(
                row![
                    text(format!("{} · {}", recipient.name, recipient.issue.label()))
                        .size(13)
                        .font(weight(iced::font::Weight::Medium))
                        .width(Fill),
                    link(
                        format!("delivery-session-{key}-{}", recipient.agent),
                        format!("Open {}", recipient.name),
                        "Open session",
                        Some(Message::OpenSession(recipient.agent.to_string())),
                        c.accent,
                    ),
                ]
                .spacing(8)
                .align_y(iced::Center),
            );
        }
        if report.omitted() > 0 {
            details = details.push(small(
                format!(
                    "{} more sessions need attention. Check their Connection details in Tools.",
                    report.omitted()
                ),
                c,
            ));
        }
        let details: Element<'static, Message> = if scroll_details {
            scrollable(details).height(180).into()
        } else {
            details.into()
        };
        body = body.push(
            container(details)
                .padding([10, 12])
                .width(Fill)
                .style(move |_| container::Style {
                    background: Some(alpha(c.text, if c.dark { 0.04 } else { 0.03 }).into()),
                    border: iced::Border {
                        color: c.line,
                        width: 1.0,
                        radius: RADIUS_MD.into(),
                    },
                    ..Default::default()
                }),
        );
    }
    Some(body.into())
}

/// How to get a session that cannot take messages taking them again, in
/// as few words as do it: for Claude Code, the one command to paste (its
/// folder, its conversation, the channel) with a copy button; for Codex,
/// the way to a session that takes them live; for the rest, one line.
pub(super) fn reconnect(
    agent: &agentdocker_core::AgentRecord,
    agents: &[agentdocker_core::AgentRecord],
    key: &str,
    c: Colors,
) -> Option<Element<'static, Message>> {
    let blocked = agentdocker_core::provider_block(agent, agents)
        .and_then(|(_, state)| state.issue.as_ref())
        .map(|issue| issue.kind);
    let issue =
        agentdocker_core::RecipientReadiness::for_agent(agent, chrono::Utc::now(), blocked)?;
    let hint = small(issue.hint(), c);
    let restart = matches!(
        issue.issue,
        agentdocker_core::SendIssue::NoReceiver
            | agentdocker_core::SendIssue::ReceiverSilent
            | agentdocker_core::SendIssue::SessionEnded
    );
    let command = restart
        .then(|| agentdocker_core::send_readiness::claude_resume_command(agent, cfg!(windows)))
        .flatten();
    if let Some(command) = command {
        return Some(
            column![
                command_well(format!("reconnect-copy-{key}-{}", agent.id), command, c),
                hint
            ]
            .spacing(6)
            .into(),
        );
    }
    if agent.spec.runtime == "codex"
        && matches!(
            issue.issue,
            agentdocker_core::SendIssue::NoReceiver | agentdocker_core::SendIssue::ReceiverSilent
        )
    {
        return Some(
            row![
                hint.width(Fill),
                action(
                    format!("reconnect-launch-{key}-{}", agent.id),
                    "Start a live Codex",
                    Some(Message::LaunchWith("codex".into())),
                    false
                ),
            ]
            .spacing(8)
            .align_y(iced::Center)
            .into(),
        );
    }
    Some(hint.into())
}

/// A command to paste, the way an approval shows one: a prompt mark, the
/// command whole in the code face on a step of tone, and a copy button
/// that copies exactly it.
fn command_well(id: String, command: String, c: Colors) -> Element<'static, Message> {
    container(
        row![
            text("$")
                .size(13)
                .line_height(iced::Pixels(20.0))
                .font(Font::MONOSPACE)
                .color(c.amber),
            text(command.clone())
                .size(12)
                .line_height(iced::Pixels(20.0))
                .font(Font::MONOSPACE)
                .color(c.text)
                .width(Fill)
                .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
            tip(
                custom_sized(
                    id,
                    "Copy command",
                    container(icon(Icon::Copy, c.muted, 14.0)).center(20),
                    Some(Message::CopyGuidance(command)),
                    false,
                    Kind::Ghost,
                    [0, 0],
                    iced::Length::Shrink,
                ),
                "Copy command",
                c,
            ),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Start),
    )
    .padding([8, 10])
    .width(Fill)
    .style(move |_| container::Style {
        background: Some(mix(c.card, c.raised, 0.6).into()),
        border: iced::Border {
            radius: RADIUS_MD.into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}

#[cfg(test)]
mod tests {
    use super::super::view::testing::seen;
    use super::*;
    use agentdocker_core::{AgentRecord, AgentSpec, AgentStatus};

    fn session(runtime: &str) -> AgentRecord {
        let mut agent = AgentRecord::new(
            AgentSpec {
                name: format!("{runtime}-1"),
                runtime: runtime.into(),
                workdir: Some("/work/orchard".into()),
                ..Default::default()
            },
            false,
            chrono::Utc::now() - chrono::Duration::minutes(10),
        );
        agent.status = AgentStatus::Running;
        agent
            .spec
            .labels
            .insert("session_id".into(), "3ebbfa91-6e52".into());
        agent
    }

    /// A Claude session that cannot be woken shows the one command to
    /// paste, whole, with a copy button, and one short line; no paragraph
    /// of instructions.
    #[test]
    fn a_claude_session_shows_its_command_and_a_copy_button() {
        let agent = session("claude-code");
        let c = Colors::new(false);
        let view = seen(reconnect(&agent, std::slice::from_ref(&agent), "inspector", c).unwrap());
        let command =
            agentdocker_core::send_readiness::claude_resume_command(&agent, cfg!(windows)).unwrap();
        assert!(view.reads(&command), "the whole command, as it is copied");
        let copy = view
            .control(&format!("reconnect-copy-inspector-{}", agent.id))
            .expect("copy");
        assert_eq!((copy.1.as_str(), copy.3), ("Copy command", true));
        assert!(view.reads("Quit it, then run this in a terminal."));
        assert!(!view.reads("Copy instructions") && !view.reads("After saving work"));
    }

    /// A Codex session says when it takes messages and offers the session
    /// that takes them live; nothing to copy that would not work.
    #[test]
    fn a_codex_session_offers_a_live_one_instead_of_instructions() {
        let agent = session("codex");
        let view = seen(
            reconnect(
                &agent,
                std::slice::from_ref(&agent),
                "inspector",
                Colors::new(true),
            )
            .unwrap(),
        );
        assert!(view.reads("It takes messages at its next prompt."));
        assert_eq!(
            view.control(&format!("reconnect-launch-inspector-{}", agent.id))
                .unwrap()
                .1,
            "Start a live Codex"
        );
        assert!(
            view.controls
                .iter()
                .all(|c| !c.0.starts_with("reconnect-copy")),
            "no copy"
        );
    }

    /// Delivery details are one line per session and the way to it.
    #[test]
    fn delivery_details_are_one_line_per_session() {
        let agent = session("claude-code");
        let mut report = agentdocker_core::SendReadiness::default();
        report.observe(agentdocker_core::RecipientReadiness::for_agent(
            &agent,
            chrono::Utc::now(),
            None,
        ));
        let mut draft = TextDraft::default();
        draft.readiness = Some(report);
        draft.readiness_expanded = true;
        let view = seen(
            notice(
                &draft,
                DeliveryTarget::Session("s".into()),
                Colors::new(false),
            )
            .unwrap(),
        );
        assert!(view.reads("claude-code-1 · "));
        assert!(
            view.control(&format!("delivery-session-session-s-{}", agent.id))
                .is_some()
        );
        assert!(!view.reads("Copy instructions") && !view.reads("After saving work"));
    }
}
