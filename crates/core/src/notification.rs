//! Stable notification destinations. Text and display names never select a route.
use serde::{Deserialize, Serialize};

use crate::{AgentId, ChannelId, MessageId, ProjectId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationTarget {
    pub message: MessageId,
    pub agent: AgentId,
    pub project: Option<ProjectId>,
    pub channel: Option<ChannelId>,
}

impl NotificationTarget {
    /// Bound metadata from OS callbacks and command-line activation before use.
    pub fn is_valid(&self) -> bool {
        let id = |value: &str| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        };
        id(self.message.as_str())
            && id(self.agent.as_str())
            && self.project.as_ref().is_none_or(|p| id(p.as_str()))
            && self.channel.as_ref().is_none_or(|c| id(c.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations_round_trip_and_refuse_paths_or_oversized_identifiers() {
        let mut target = NotificationTarget {
            message: MessageId::from("message-1".to_owned()),
            agent: AgentId::from("agent-1"),
            project: Some(ProjectId::from("project-1")),
            channel: Some(ChannelId::from("channel-1")),
        };
        assert!(target.is_valid());
        assert_eq!(
            serde_json::from_str::<NotificationTarget>(&serde_json::to_string(&target).unwrap())
                .unwrap(),
            target
        );
        for invalid in ["", "../another-project", "agent\nopen", &"x".repeat(129)] {
            target.agent = AgentId::from(invalid);
            assert!(!target.is_valid());
        }
    }
}
