//! Messages exchanged between agents.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::AgentId;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageId(String);

impl MessageId {
    pub fn generate() -> Self {
        let raw = uuid::Uuid::new_v4().simple().to_string();
        Self(raw[..16].to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for MessageId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a message goes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Destination {
    /// One agent. Clients may address by name; the daemon resolves to an id
    /// before publishing.
    Agent(AgentId),
    /// Every live subscriber of a topic such as `repo/backend/reviews`.
    /// Subscribers use MQTT-style patterns (`+` one level, `#` the rest).
    Topic(String),
    /// Every live agent.
    Broadcast,
}

impl Destination {
    /// Parse the shorthand accepted on the command line:
    /// `all` / `*` → broadcast, `topic:x/y` → topic, anything else → agent.
    pub fn parse(s: &str) -> Self {
        match s {
            "all" | "*" => Self::Broadcast,
            _ => match s.strip_prefix("topic:") {
                Some(topic) => Self::Topic(topic.to_owned()),
                None => Self::Agent(AgentId::from(s)),
            },
        }
    }
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Agent(id) => f.write_str(id.short()),
            Self::Topic(topic) => write!(f, "topic:{topic}"),
            Self::Broadcast => f.write_str("all"),
        }
    }
}

/// The unit of communication between agents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub id: MessageId,
    /// Sender: an agent id, or `user` for messages injected from the CLI.
    pub from: String,
    pub to: Destination,
    /// Application-level type: `chat`, `task`, `handoff`, `question`,
    /// `answer`, `notice`... Agents agree on kinds; the daemon just routes.
    pub kind: String,
    pub payload: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<MessageId>,
    pub sent_at: DateTime<Utc>,
}

impl Envelope {
    pub fn new(
        from: impl Into<String>,
        to: Destination,
        kind: impl Into<String>,
        payload: serde_json::Value,
        reply_to: Option<MessageId>,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            id: MessageId::generate(),
            from: from.into(),
            to,
            kind: kind.into(),
            payload,
            reply_to,
            sent_at: now,
        }
    }
}

/// MQTT-style topic matching: `+` matches exactly one level, `#` matches the
/// remainder (including nothing).
pub fn topic_matches(pattern: &str, topic: &str) -> bool {
    let mut pattern = pattern.split('/');
    let mut topic = topic.split('/');
    loop {
        match (pattern.next(), topic.next()) {
            (Some("#"), _) => return true,
            (Some("+"), Some(_)) => continue,
            (Some(a), Some(b)) if a == b => continue,
            (None, None) => return true,
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_patterns() {
        assert!(topic_matches("a/b/c", "a/b/c"));
        assert!(!topic_matches("a/b/c", "a/b"));
        assert!(!topic_matches("a/b", "a/b/c"));
        assert!(topic_matches("a/+/c", "a/x/c"));
        assert!(!topic_matches("a/+/c", "a/x/y/c"));
        assert!(topic_matches("a/#", "a/x/y/c"));
        assert!(topic_matches("a/#", "a"));
        assert!(topic_matches("#", "anything/at/all"));
        assert!(!topic_matches("b/#", "a/b"));
    }

    #[test]
    fn destination_shorthand() {
        assert_eq!(Destination::parse("all"), Destination::Broadcast);
        assert_eq!(Destination::parse("*"), Destination::Broadcast);
        assert_eq!(
            Destination::parse("topic:repo/reviews"),
            Destination::Topic("repo/reviews".into())
        );
        assert_eq!(
            Destination::parse("reviewer"),
            Destination::Agent(AgentId::from("reviewer"))
        );
    }

    #[test]
    fn destination_serialises_tagged() {
        let json = serde_json::to_string(&Destination::Topic("x".into())).unwrap();
        assert_eq!(json, r#"{"kind":"topic","value":"x"}"#);
        let json = serde_json::to_string(&Destination::Broadcast).unwrap();
        assert_eq!(json, r#"{"kind":"broadcast"}"#);
    }
}
