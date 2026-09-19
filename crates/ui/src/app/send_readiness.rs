//! Compact send-time warnings belong to the draft destination they answered.
use super::{
    Message,
    shell::{ChannelDraft, DeliveryTarget},
    style::Colors,
    view::small,
};
use crate::controls::button as action;
use iced::{
    Element, Fill,
    widget::{column, row, scrollable, text},
};

pub(super) fn notice(
    draft: &ChannelDraft,
    target: DeliveryTarget,
    c: Colors,
) -> Option<Element<'static, Message>> {
    notice_content(draft, target, c, true)
}

/// A composer already scrolls all its feedback within the available pane.
pub(super) fn composer_notice(
    draft: &ChannelDraft,
    target: DeliveryTarget,
    c: Colors,
) -> Option<Element<'static, Message>> {
    notice_content(draft, target, c, false)
}

fn notice_content(
    draft: &ChannelDraft,
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
        DeliveryTarget::Channel(key) => format!("channel-{key}"),
    };
    let mut body = column![
        row![
            text(format!(
                "Queued · {} {}",
                report.needs_attention,
                if report.needs_attention == 1 {
                    "session needs attention"
                } else {
                    "sessions need attention"
                }
            ))
            .size(13)
            .color(c.amber)
            .width(Fill),
            action(
                format!("delivery-details-{key}"),
                if draft.readiness_expanded {
                    "Hide details"
                } else {
                    "Delivery details"
                },
                Some(Message::DeliveryDetails(target)),
                false
            ),
        ]
        .spacing(6)
    ]
    .spacing(4);
    if draft.readiness_expanded {
        let mut details = column![small(
            "Readiness when sent. Queue acceptance is not a provider receipt.",
            c
        )]
        .spacing(8);
        for recipient in &report.details {
            let guidance = recipient.guidance();
            details = details.push(
                column![
                    text(format!("{} · {}", recipient.name, recipient.issue.label())).size(13),
                    small(guidance.clone(), c),
                    row![
                        action(
                            format!("delivery-session-{key}-{}", recipient.agent),
                            "Open session",
                            Some(Message::OpenSession(recipient.agent.to_string())),
                            false
                        ),
                        action(
                            format!("delivery-copy-{key}-{}", recipient.agent),
                            "Copy instructions",
                            Some(Message::CopyGuidance(guidance)),
                            false
                        ),
                    ]
                    .spacing(6)
                ]
                .spacing(3),
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
        body = if scroll_details {
            body.push(scrollable(details).height(180))
        } else {
            body.push(details)
        };
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
