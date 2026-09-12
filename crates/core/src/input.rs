//! Durable delivery evidence. Queueing, provider receipt and task completion
//! are separate facts; neither a socket write nor an empty inbox is a receipt.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::MessageId;

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
    Paused,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputDelivery {
    /// The process generation that made the most recent report.
    pub process_started_at: DateTime<Utc>,
    pub paused: bool,
    pub reported_at: DateTime<Utc>,
    /// The latest receipt remains historical evidence across restarts.
    pub received: Option<ReceivedInput>,
    pub received_at: Option<DateTime<Utc>>,
}

impl InputDelivery {
    pub fn paused_for(&self, process_started_at: Option<DateTime<Utc>>) -> bool {
        self.paused && process_started_at == Some(self.process_started_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
