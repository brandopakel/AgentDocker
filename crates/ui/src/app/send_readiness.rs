//! Compact send-time warnings belong to the draft destination they answered.
use super::{
    Message,
    messages::look::{link, status_line},
    shell::{DeliveryTarget, TextDraft},
    style::{Colors, RADIUS_MD, alpha, weight},
    view::small,
};
use crate::controls::button as action;
use iced::{
    Element, Fill,
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
        let mut details = column![small(
            "As it was when sent. Sent means waiting for the agent, not yet read.",
            c
        )]
        .spacing(10);
        for recipient in &report.details {
            let guidance = recipient.guidance();
            details = details.push(
                column![
                    text(format!("{} · {}", recipient.name, recipient.issue.label()))
                        .size(13)
                        .font(weight(iced::font::Weight::Medium)),
                    small(guidance.clone(), c),
                    row![
                        link(
                            format!("delivery-session-{key}-{}", recipient.agent),
                            "Open session",
                            "Open session",
                            Some(Message::OpenSession(recipient.agent.to_string())),
                            c.accent,
                        ),
                        link(
                            format!("delivery-copy-{key}-{}", recipient.agent),
                            "Copy instructions",
                            "Copy instructions",
                            Some(Message::CopyGuidance(guidance)),
                            c.accent,
                        ),
                    ]
                    .spacing(2)
                ]
                .spacing(4),
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

/// Guidance is visible only inside details the person explicitly opened.
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
    let guidance = issue.guidance();
    Some(
        column![
            small(guidance.clone(), c),
            action(
                format!("reconnect-copy-{key}-{}", agent.id),
                "Copy instructions",
                Some(Message::CopyGuidance(guidance)),
                false
            ),
        ]
        .spacing(6)
        .into(),
    )
}
