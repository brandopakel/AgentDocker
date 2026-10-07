//! Where one of the person's messages stands with each agent it was queued
//! for. Every state is something the daemon saw; none says a model read it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AgentId, AgentRecord, InputReadiness, MessageId, ProviderIssueKind};

/// One recipient of a message and how far it got.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipient {
    pub agent: AgentId,
    /// The agent's name and runtime, while its record is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    pub queued_at: DateTime<Utc>,
    /// When its session first read the message from its queue without
    /// taking it — put before the model, not confirmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offered_at: Option<DateTime<Utc>>,
    /// When the provider confirmed the model received it (a Claude channel
    /// or Codex receipt naming it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_at: Option<DateTime<Utc>>,
    /// When it left the agent's queue: taken by the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taken_at: Option<DateTime<Utc>>,
    /// The agent's first reply to it, in any conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<MessageId>,
    pub state: State,
}

/// The furthest a message got with one agent, first match wins: answered,
/// taken, received, shown, else why it is still waiting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Answered,
    Delivered,
    Received,
    Shown,
    /// Queued for a session nothing wakes: it takes it at its next prompt.
    WaitingForPrompt,
    /// Queued for a browser agent, which reads only when it checks in.
    WaitsForCheckIn,
    /// Queued for a session whose receiver is live.
    WaitingToTake,
    Paused,
    Blocked,
    /// Queued for a session whose receiver has gone quiet.
    Silent,
    /// The session ended with the message still queued.
    Ended,
    /// The agent's record is gone.
    Gone,
    /// A state from a newer daemon.
    #[serde(other)]
    Unknown,
}

impl State {
    /// Where the message stands with `agent`, from what the daemon saw:
    /// `agent` is its record if still known, `blocked` its provider's
    /// standing block if any.
    pub fn of(
        agent: Option<&AgentRecord>,
        facts: &Recipient,
        blocked: Option<ProviderIssueKind>,
        now: DateTime<Utc>,
    ) -> Self {
        if facts.reply.is_some() {
            return Self::Answered;
        }
        if facts.taken_at.is_some() {
            return Self::Delivered;
        }
        if facts.received_at.is_some() {
            return Self::Received;
        }
        if facts.offered_at.is_some() {
            return Self::Shown;
        }
        let Some(agent) = agent else {
            return Self::Gone;
        };
        if !agent.status.is_live() {
            return Self::Ended;
        }
        if blocked.is_some() {
            return Self::Blocked;
        }
        if agent.via_connector() {
            return Self::WaitsForCheckIn;
        }
        match InputReadiness::for_agent(agent, now) {
            InputReadiness::SessionEnded => Self::Ended,
            InputReadiness::Unverified => Self::WaitingForPrompt,
            InputReadiness::Paused => Self::Paused,
            InputReadiness::Stale => Self::Silent,
            InputReadiness::AwaitingFirstReceipt | InputReadiness::Verified => Self::WaitingToTake,
        }
    }

    /// The words under the person's message.
    pub fn label(self) -> &'static str {
        match self {
            Self::Answered => "answered",
            Self::Delivered => "delivered",
            Self::Received => "received by the model",
            Self::Shown => "shown to its session, not confirmed",
            Self::WaitingForPrompt => "waiting for its next prompt",
            Self::WaitsForCheckIn => "waits until it checks in",
            Self::WaitingToTake => "waiting for it to take it",
            Self::Paused => "delivery paused",
            Self::Blocked => "its provider is blocked",
            Self::Silent => "its receiver has gone quiet",
            Self::Ended => "session ended before taking it",
            Self::Gone => "no longer known",
            Self::Unknown => "unknown",
        }
    }

    /// Whether it can change no further: answered, or the agent can no
    /// longer take it.
    pub fn settled(self) -> bool {
        matches!(self, Self::Answered | Self::Ended | Self::Gone)
    }

    /// Whether the message has reached the agent's session at all.
    pub fn reached(self) -> bool {
        matches!(
            self,
            Self::Answered | Self::Delivered | Self::Received | Self::Shown
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentSpec, AgentStatus};

    fn recipient() -> Recipient {
        Recipient {
            agent: AgentId::from("agent"),
            name: None,
            runtime: None,
            queued_at: Utc::now(),
            offered_at: None,
            received_at: None,
            taken_at: None,
            reply: None,
            state: State::Unknown,
        }
    }

    fn live(runtime: &str) -> AgentRecord {
        let mut record = AgentRecord::new(
            AgentSpec {
                name: "agent".into(),
                runtime: runtime.into(),
                ..AgentSpec::default()
            },
            false,
            Utc::now(),
        );
        record.status = AgentStatus::Running;
        record
    }

    /// The furthest step the daemon saw wins, and a message still queued
    /// says why it waits; nothing is said that the daemon did not see.
    #[test]
    fn a_message_stands_at_the_furthest_step_the_daemon_saw() {
        let now = Utc::now();
        let agent = live("claude-code");
        let mut facts = recipient();
        assert_eq!(
            State::of(Some(&agent), &facts, None, now),
            State::WaitingForPrompt,
            "no receiver: it waits for the next prompt"
        );
        facts.offered_at = Some(now);
        assert_eq!(State::of(Some(&agent), &facts, None, now), State::Shown);
        facts.received_at = Some(now);
        assert_eq!(State::of(Some(&agent), &facts, None, now), State::Received);
        facts.taken_at = Some(now);
        assert_eq!(State::of(Some(&agent), &facts, None, now), State::Delivered);
        facts.reply = Some(MessageId::from("reply".to_owned()));
        assert_eq!(State::of(Some(&agent), &facts, None, now), State::Answered);

        let queued = recipient();
        assert_eq!(State::of(None, &queued, None, now), State::Gone);
        let mut ended = live("codex");
        ended.status = AgentStatus::Exited { code: Some(0) };
        assert_eq!(State::of(Some(&ended), &queued, None, now), State::Ended);
        assert_eq!(
            State::of(Some(&agent), &queued, Some(ProviderIssueKind::Rate), now),
            State::Blocked
        );
        let mut browser = live("claude-browser");
        browser
            .spec
            .labels
            .insert(crate::agent::CONNECTOR_LABEL.into(), "true".into());
        assert_eq!(
            State::of(Some(&browser), &queued, None, now),
            State::WaitsForCheckIn
        );
        assert!(State::Shown.reached() && !State::WaitingForPrompt.reached());
        let newer: State = serde_json::from_str("\"read_by_human\"").unwrap();
        assert_eq!(newer, State::Unknown);
    }
}
