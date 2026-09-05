//! Agents: the specs that describe them and the records that track them.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ProjectRef;

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

/// Which branch and commit an agent's checkout is on, as last observed —
/// by the daemon reading `.git` on a timer, or reported by a hook the
/// moment it sees a tool run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VcsState {
    /// `None` when HEAD is detached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The commit HEAD points at; `None` on an unborn branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// Uncommitted changes, when something cheap can tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
    pub updated_at: DateTime<Utc>,
}

impl VcsState {
    /// Same branch, commit, and dirtiness — the timestamp does not count.
    pub fn same_as(&self, other: &VcsState) -> bool {
        self.branch == other.branch && self.head == other.head && self.dirty == other.dirty
    }

    /// The first seven characters of the commit, as git prints it.
    pub fn short_head(&self) -> Option<&str> {
        self.head.as_deref().map(|head| {
            let end = head.char_indices().nth(7).map_or(head.len(), |(i, _)| i);
            &head[..end]
        })
    }

    /// `main@3f9c1e0`, `(detached)@3f9c1e0`, or `main (unborn)`.
    pub fn describe(&self) -> String {
        match (&self.branch, self.short_head()) {
            (Some(branch), Some(head)) => format!("{branch}@{head}"),
            (None, Some(head)) => format!("(detached)@{head}"),
            (Some(branch), None) => format!("{branch} (unborn)"),
            (None, None) => "-".to_owned(),
        }
    }
}

/// A running agent process nobody has registered, as `discover` reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredProcess {
    pub pid: u32,
    pub ppid: u32,
    /// From the known-runtime table: `claude-code`, `codex`, ...
    pub runtime: String,
    /// The command line, as `ps` shows it.
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// The project containing `cwd`, without a fingerprint — the id is
    /// assigned when the process is adopted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
}

impl DiscoveredProcess {
    /// The name `adopt` gives the agent unless told otherwise.
    pub fn default_name(&self) -> String {
        format!("{}-{}", self.runtime, self.pid)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AgentStatus {
    Created,
    Running,
    /// A stop signal was sent; exit has not yet been observed.
    Stopping,
    Exited {
        code: Option<i32>,
    },
    Failed {
        reason: String,
    },
}

impl AgentStatus {
    /// Created or running: the agent still counts as present.
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Created | Self::Running | Self::Stopping)
    }
}

impl fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Created => f.write_str("created"),
            Self::Running => f.write_str("running"),
            Self::Stopping => f.write_str("stopping"),
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
    /// Dedicated process group created by agentd for a managed command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_group: Option<u32>,
    /// `true` when agentd spawned the process, `false` when an external
    /// process registered itself.
    pub managed: bool,
    /// The project derived from `spec.workdir` when the agent was created;
    /// `None` when there was no working directory to derive it from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectRef>,
    /// Branch and head of the checkout, when the agent has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcs: Option<VcsState>,
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
            process_group: None,
            managed,
            project: None,
            vcs: None,
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
