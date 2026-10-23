//! Where one of the person's messages stands with each agent it was queued
//! for. Every state is something the daemon saw; none says a model read it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AgentId, AgentRecord, MessageId, ProviderIssueKind};

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
    /// Queued for a session nothing of which has reached AgentDocker.
    Unreachable,
    /// Queued for a session nothing wakes: it takes it at its next prompt.
    WaitingForPrompt,
    /// Queued for a browser agent, or a session away for hours, which
    /// reads only when it checks in.
    WaitsForCheckIn,
    /// Queued for a session whose receiver is live.
    WaitingToTake,
    /// Queued for a session whose receiver holds its input while the person
    /// types in its terminal ([`crate::typing`]).
    HeldForTyping,
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
        // Why it still waits is the one readiness decision, said here in
        // the words under a message.
        use crate::readiness::State as Readiness;
        match crate::readiness::decide(agent, now, blocked).state {
            Readiness::Ended => Self::Ended,
            Readiness::ProviderBlocked(_) => Self::Blocked,
            Readiness::Paused => Self::Paused,
            Readiness::Silent => Self::Silent,
            Readiness::Unreachable => Self::Unreachable,
            Readiness::AwaitsCheckIn => Self::WaitsForCheckIn,
            Readiness::WaitsForPrompt => Self::WaitingForPrompt,
            Readiness::Ready { .. } => Self::WaitingToTake,
            Readiness::HeldForTyping => Self::HeldForTyping,
        }
    }

    /// The words under the person's message.
    pub fn label(self) -> &'static str {
        match self {
            Self::Answered => "answered",
            Self::Delivered => "delivered",
            Self::Received => "received by the model",
            Self::Shown => "shown to its session, not confirmed",
            Self::Unreachable => "nothing connects its session to AgentDocker",
            Self::WaitingForPrompt => "waiting for its next prompt",
            Self::WaitsForCheckIn => "waits until it checks in",
            Self::WaitingToTake => "waiting for it to take it",
            Self::HeldForTyping => "waiting for you to stop typing in its terminal",
            Self::Paused => "delivery paused",
            Self::Blocked => "its provider is blocked",
            Self::Silent => "its receiver has gone quiet",
            Self::Ended => "session ended before taking it",
            Self::Gone => "no longer known",
            Self::Unknown => "unknown",
        }
    }

    /// The state in a word or two, for the chip under the message; the
    /// [`label`](Self::label) is its detail. States that differ only in why
    /// a message still waits share *Waiting*, and the two that end without
    /// the message taken share *Not delivered*.
    pub fn word(self) -> &'static str {
        match self {
            Self::Answered => "Answered",
            Self::Delivered => "Delivered",
            Self::Received => "Received",
            Self::Shown => "Shown",
            Self::Unreachable => "Can't receive",
            Self::WaitingForPrompt
            | Self::WaitsForCheckIn
            | Self::WaitingToTake
            | Self::HeldForTyping => "Waiting",
            Self::Paused => "Paused",
            Self::Blocked => "Provider blocked",
            Self::Silent => "Not heard from",
            Self::Ended | Self::Gone => "Not delivered",
            Self::Unknown => "Unknown",
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
        // A session known by its key alone and away for hours reads when
        // it is back, as the readiness decision says.
        let mut away = live("codex");
        away.spec
            .labels
            .insert("session_id".into(), "thread".into());
        away.session_contact = Some(crate::agent::SessionContact {
            adapter: crate::AdapterKind::Hooks,
            observed_at: now - crate::agent::SESSION_PRESENCE - chrono::Duration::minutes(1),
        });
        assert_eq!(
            State::of(Some(&away), &queued, None, now),
            State::WaitsForCheckIn
        );
        assert_eq!(
            crate::readiness::decide(&away, now, None).state,
            crate::readiness::State::AwaitsCheckIn
        );
        // Its receiver holds input while the person types in its terminal.
        let mut held = live("codex");
        held.process_started_at = Some(now - chrono::Duration::minutes(1));
        held.input_delivery = Some(crate::InputDelivery {
            process_started_at: now - chrono::Duration::minutes(1),
            paused: false,
            pause_reason: None,
            reported_at: now,
            received: None,
            received_at: None,
            typing_until: Some(now + crate::typing::QUIET),
        });
        assert_eq!(
            State::of(Some(&held), &queued, None, now),
            State::HeldForTyping
        );
        assert_eq!(
            State::of(Some(&held), &queued, None, now + crate::typing::QUIET),
            State::WaitingToTake
        );
        assert!(State::Shown.reached() && !State::WaitingForPrompt.reached());
        assert_eq!(State::WaitsForCheckIn.word(), "Waiting");
        assert_eq!(State::Gone.word(), State::Ended.word());
        let newer: State = serde_json::from_str("\"read_by_human\"").unwrap();
        assert_eq!(newer, State::Unknown);
    }
}
