//! Agents: the specs that describe them and the records that track them.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Unique identifier of an agent instance. Like a Docker container ID: a
/// random hex string that can be abbreviated to any unique prefix.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentId(String);

impl AgentId {
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Abbreviated form for tables, like the 12-character IDs in `docker ps`.
    pub fn short(&self) -> &str {
        let end = self
            .0
            .char_indices()
            .nth(12)
            .map_or(self.0.len(), |(i, _)| i);
        &self.0[..end]
    }
}

impl From<String> for AgentId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for AgentId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Immutable description of how an agent is created — the "image".
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSpec {
    /// Human-friendly name, unique among live agents.
    pub name: String,
    /// Runtime hosting the agent: `claude-code`, `codex`, `gemini-cli`,
    /// `cursor`, `custom`, ... Free-form so new runtimes need no code change.
    #[serde(default)]
    pub runtime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Command line that launches the agent. Empty for externally managed
    /// agents that only register themselves.
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AgentStatus {
    Created,
    Running,
    Exited { code: Option<i32> },
    Failed { reason: String },
}

impl AgentStatus {
    /// Created or running: the agent still counts as present.
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Created | Self::Running)
    }
}

impl fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Created => f.write_str("created"),
            Self::Running => f.write_str("running"),
            Self::Exited { code: Some(code) } => write!(f, "exited ({code})"),
            Self::Exited { code: None } => f.write_str("exited (signal)"),
            Self::Failed { reason } => write!(f, "failed: {reason}"),
        }
    }
}

/// Everything the daemon knows about one agent instance — the "container".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRecord {
    pub id: AgentId,
    pub spec: AgentSpec,
    pub status: AgentStatus,
    /// Host that supervises the agent. Always `local` until federation lands.
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// When the process behind `pid` started, so a recycled pid is not
    /// mistaken for the agent. `None` when the platform can't tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_started_at: Option<DateTime<Utc>>,
    /// `true` when agentd spawned the process, `false` when an external
    /// process registered itself.
    pub managed: bool,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    pub last_seen: DateTime<Utc>,
}

impl AgentRecord {
    pub fn new(spec: AgentSpec, managed: bool, now: DateTime<Utc>) -> Self {
        Self {
            id: AgentId::generate(),
            spec,
            status: AgentStatus::Created,
            host: "local".to_owned(),
            pid: None,
            process_started_at: None,
            managed,
            created_at: now,
            started_at: None,
            finished_at: None,
            last_seen: now,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_id_is_twelve_chars() {
        let id = AgentId::generate();
        assert_eq!(id.short().len(), 12);
        assert!(id.as_str().starts_with(id.short()));
        assert_eq!(AgentId::from("abc").short(), "abc");
    }

    #[test]
    fn status_liveness() {
        assert!(AgentStatus::Created.is_live());
        assert!(AgentStatus::Running.is_live());
        assert!(!AgentStatus::Exited { code: Some(0) }.is_live());
        assert!(!AgentStatus::Failed { reason: "x".into() }.is_live());
    }

    #[test]
    fn status_serialises_with_state_tag() {
        let json = serde_json::to_string(&AgentStatus::Exited { code: Some(1) }).unwrap();
        assert_eq!(json, r#"{"state":"exited","code":1}"#);
    }
}
