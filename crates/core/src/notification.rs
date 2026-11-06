//! Stable notification destinations, and the ledger that tells each state
//! once. Text and display names never select a route.
use std::collections::BTreeMap;

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
        valid_id(self.message.as_str())
            && valid_id(self.agent.as_str())
            && self.project.as_ref().is_none_or(|p| valid_id(p.as_str()))
            && self.channel.as_ref().is_none_or(|c| valid_id(c.as_str()))
    }
}

/// An identifier a notification may carry: short, and nothing a path or
/// a shell could read as its own.
pub fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}

/// What has been told, so one state is told once. A poster keys each
/// notice by its tag — the subject: a question, a window's notice about
/// an agent — and by its state, what is said of that subject now (a
/// prompt and its detail, a finish). A state the ledger already holds
/// under its tag is not news and is not posted again; a new state under
/// an old tag is, and replaces the last. Forgetting a tag, once its
/// notice is taken back, lets the same state be told afresh when it
/// comes again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Told {
    states: BTreeMap<String, String>,
}

impl Told {
    /// Whether `state` is news under `tag`, recording it when it is.
    pub fn news(&mut self, tag: &str, state: &str) -> bool {
        if self.states.get(tag).is_some_and(|told| told == state) {
            return false;
        }
        self.states.insert(tag.to_owned(), state.to_owned());
        true
    }

    /// The notice under `tag` was taken back: nothing is told of it now.
    pub fn forget(&mut self, tag: &str) {
        self.states.remove(tag);
    }

    /// What is told under `tag`, if anything.
    pub fn state(&self, tag: &str) -> Option<&str> {
        self.states.get(tag).map(String::as_str)
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

    /// The same state under one tag is told once; a changed state is news
    /// and replaces it; a tag taken back is told afresh.
    #[test]
    fn a_state_is_told_once_until_it_changes_or_is_taken_back() {
        let mut told = Told::default();
        assert!(told.news("window.agent-1", "waiting:permission"));
        assert!(
            !told.news("window.agent-1", "waiting:permission"),
            "the same prompt again is not news"
        );
        assert_eq!(told.state("window.agent-1"), Some("waiting:permission"));
        assert!(
            told.news("window.agent-1", "waiting:question"),
            "a changed ask is news"
        );
        assert!(!told.news("window.agent-1", "waiting:question"));
        assert!(
            told.news("window.agent-2", "waiting:question"),
            "another subject is its own"
        );
        told.forget("window.agent-1");
        assert_eq!(told.state("window.agent-1"), None);
        assert!(
            told.news("window.agent-1", "waiting:question"),
            "taken back, the same state is told again when it comes"
        );
    }
}
