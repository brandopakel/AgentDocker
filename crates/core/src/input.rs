//! Durable delivery evidence. Queueing, provider receipt and task completion
//! are separate facts; neither a socket write nor an empty inbox is a receipt.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::MessageId;

/// Evidence about a session's input receiver, not receipt of any particular
/// message or proof that its provider is available. Provider limits are separate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputReadiness {
    SessionEnded,
    Unverified,
    Paused,
    Stale,
    AwaitingFirstReceipt,
    Verified,
}

impl InputReadiness {
    pub fn for_agent(agent: &crate::AgentRecord, now: DateTime<Utc>) -> Self {
        if !agent.status.is_live() {
            return Self::SessionEnded;
        }
        let Some(delivery) = agent.input_delivery.as_ref() else {
            return Self::Unverified;
        };
        if delivery.paused_for(agent.process_started_at) {
            return Self::Paused;
        }
        if !delivery.current_for(agent.process_started_at, now) {
            return Self::Stale;
        }
        if delivery.received_for(agent.process_started_at, now) {
            Self::Verified
        } else {
            Self::AwaitingFirstReceipt
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::SessionEnded => "Session ended",
            Self::Unverified => "Idle delivery not verified",
            Self::Paused => "Delivery paused",
            Self::Stale => "No recent receiver signal",
            Self::AwaitingFirstReceipt => "Receiver active, awaiting first receipt",
            Self::Verified => "Delivery verified",
        }
    }
}

/// Contact with a particular adapter is separate from generic agent activity.
/// These observations contain no provider configuration or message contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterKind {
    Mcp,
    Hooks,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterContact {
    pub process_started_at: DateTime<Utc>,
    pub observed_at: DateTime<Utc>,
}

impl AdapterContact {
    pub fn current_for(&self, generation: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
        generation == Some(self.process_started_at)
            && self.observed_at >= self.process_started_at
            && self.observed_at <= now
            && now - self.observed_at < chrono::Duration::minutes(5)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputReceipt {
    Codex {
        thread: String,
        turn: String,
        item: String,
    },
    ClaudeChannel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceivedInput {
    pub messages: Vec<MessageId>,
    pub receipt: InputReceipt,
}

impl ReceivedInput {
    pub fn valid(&self) -> bool {
        fn id(value: &str) -> bool {
            !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
        }
        !self.messages.is_empty()
            && self.messages.len() <= 1000
            && self.messages.iter().all(|message| id(message.as_str()))
            && self
                .messages
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                == self.messages.len()
            && match &self.receipt {
                InputReceipt::Codex { thread, turn, item } => {
                    self.messages.len() == 1 && id(thread) && id(turn) && id(item)
                }
                InputReceipt::ClaudeChannel => true,
            }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputReport {
    Ready,
    Received { input: ReceivedInput },
    Paused { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputDelivery {
    /// The process generation that made the most recent report.
    pub process_started_at: DateTime<Utc>,
    pub paused: bool,
    pub pause_reason: Option<String>,
    pub reported_at: DateTime<Utc>,
    /// The latest receipt remains historical evidence across restarts.
    pub received: Option<ReceivedInput>,
    pub received_at: Option<DateTime<Utc>>,
}

impl InputDelivery {
    pub fn paused_for(&self, process_started_at: Option<DateTime<Utc>>) -> bool {
        self.paused && process_started_at == Some(self.process_started_at)
    }

    /// A receiver refreshes its report while servicing input. An old receipt,
    /// a reused PID or a silent/stopped controller cannot establish readiness.
    pub fn current_for(&self, generation: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
        !self.paused
            && generation == Some(self.process_started_at)
            && self.reported_at >= self.process_started_at
            && self.reported_at <= now
            && now - self.reported_at < chrono::Duration::seconds(90)
    }

    pub fn received_for(&self, generation: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
        generation == Some(self.process_started_at)
            && self.received.is_some()
            && self
                .received_at
                .is_some_and(|at| at >= self.process_started_at && at <= now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receiver_evidence_is_independent_of_vendor_and_rechecked_after_restart_or_silence() {
        let birth = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        for runtime in ["codex", "claude-code", "gemini-cli", "cursor", "custom"] {
            let mut agent = crate::AgentRecord::new(
                crate::AgentSpec {
                    runtime: runtime.into(),
                    ..Default::default()
                },
                false,
                birth,
            );
            agent.status = crate::AgentStatus::Running;
            agent.process_started_at = Some(birth);
            for adapter in [AdapterKind::Mcp, AdapterKind::Hooks] {
                agent.adapter_contacts.insert(
                    adapter,
                    AdapterContact {
                        process_started_at: birth,
                        observed_at: birth,
                    },
                );
            }
            assert_eq!(
                InputReadiness::for_agent(&agent, birth),
                InputReadiness::Unverified
            );
            agent.input_delivery = Some(InputDelivery {
                process_started_at: birth,
                paused: false,
                pause_reason: None,
                reported_at: birth,
                received: None,
                received_at: None,
            });
            assert_eq!(
                InputReadiness::for_agent(&agent, birth),
                InputReadiness::AwaitingFirstReceipt
            );
            let delivery = agent.input_delivery.as_mut().unwrap();
            delivery.received = Some(ReceivedInput {
                messages: vec!["previous-message".to_owned().into()],
                receipt: InputReceipt::ClaudeChannel,
            });
            delivery.received_at = Some(birth);
            assert_eq!(
                InputReadiness::for_agent(&agent, birth),
                InputReadiness::Verified
            );
            assert_eq!(
                InputReadiness::for_agent(&agent, birth + chrono::Duration::seconds(90)),
                InputReadiness::Stale
            );
            agent.process_started_at = Some(birth + chrono::Duration::seconds(1));
            assert_eq!(
                InputReadiness::for_agent(&agent, birth),
                InputReadiness::Stale
            );
            agent.process_started_at = Some(birth);
            agent.input_delivery.as_mut().unwrap().paused = true;
            assert_eq!(
                InputReadiness::for_agent(&agent, birth),
                InputReadiness::Paused
            );
            agent.status = crate::AgentStatus::Exited { code: Some(0) };
            assert_eq!(
                InputReadiness::for_agent(&agent, birth),
                InputReadiness::SessionEnded
            );
        }
    }

    #[test]
    fn contact_and_receiver_readiness_require_fresh_generation_bound_evidence() {
        let birth = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let contact = AdapterContact {
            process_started_at: birth,
            observed_at: birth,
        };
        for age in [0, 299] {
            assert!(contact.current_for(Some(birth), birth + chrono::Duration::seconds(age)));
        }
        for age in [-1, 300] {
            assert!(!contact.current_for(Some(birth), birth + chrono::Duration::seconds(age)));
        }
        assert!(!contact.current_for(None, birth));
        assert!(!contact.current_for(Some(birth + chrono::Duration::seconds(1)), birth));
        let mut delivery = InputDelivery {
            process_started_at: birth,
            paused: false,
            pause_reason: None,
            reported_at: birth,
            received: None,
            received_at: None,
        };
        assert!(delivery.current_for(Some(birth), birth + chrono::Duration::seconds(89)));
        assert!(!delivery.current_for(Some(birth), birth + chrono::Duration::seconds(90)));
        assert!(!delivery.current_for(Some(birth), birth - chrono::Duration::seconds(1)));
        assert!(!delivery.current_for(None, birth));
        assert!(!delivery.received_for(Some(birth), birth));
        delivery.received = Some(ReceivedInput {
            messages: vec!["receipt".to_owned().into()],
            receipt: InputReceipt::ClaudeChannel,
        });
        delivery.received_at = Some(birth - chrono::Duration::seconds(1));
        assert!(
            !delivery.received_for(Some(birth), birth),
            "historical receipt is not current-process evidence"
        );
        delivery.received_at = Some(birth);
        assert!(delivery.received_for(Some(birth), birth));
        assert!(!delivery.received_for(Some(birth + chrono::Duration::seconds(1)), birth));
        assert!(!delivery.received_for(None, birth));
        delivery.paused = true;
        assert!(!delivery.current_for(Some(birth), birth));
        assert!(
            delivery.received_for(Some(birth), birth),
            "pausing retains the receipt as history"
        );
    }

    #[test]
    fn receipts_require_bounded_unique_ids_and_one_codex_item() {
        let mut input = ReceivedInput {
            messages: vec!["message".to_owned().into()],
            receipt: InputReceipt::Codex {
                thread: "thread".into(),
                turn: "turn".into(),
                item: "item".into(),
            },
        };
        assert!(input.valid());
        input.messages.push("another".to_owned().into());
        assert!(!input.valid(), "one Codex item cannot prove two inputs");
        input.receipt = InputReceipt::ClaudeChannel;
        assert!(input.valid());
        input.messages.push("message".to_owned().into());
        assert!(!input.valid());
        for bad in ["".to_owned(), "a".repeat(129), "line\nbreak".into()] {
            input.messages = vec![bad.into()];
            assert!(!input.valid());
        }
        input.messages = (0..1001).map(|i| i.to_string().into()).collect();
        assert!(!input.valid());
    }

    #[test]
    fn legacy_agent_and_activity_do_not_invent_delivery_evidence() {
        let record = crate::AgentRecord::new(
            crate::AgentSpec::default(),
            false,
            DateTime::from_timestamp(1, 0).unwrap(),
        );
        let mut value = serde_json::to_value(record).unwrap();
        value.as_object_mut().unwrap().remove("input_delivery");
        assert!(
            serde_json::from_value::<crate::AgentRecord>(value)
                .unwrap()
                .input_delivery
                .is_none()
        );
        let activity: crate::AgentActivity = serde_json::from_value(
            serde_json::json!({"agent":"id", "name":"agent", "activity":{"state":"unknown"}}),
        )
        .unwrap();
        assert_eq!(activity.queued_inputs, None);
    }
}
