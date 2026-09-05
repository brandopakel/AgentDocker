//! The attribution ledger: one entry per file change the project watcher
//! saw, with whoever held the file at the time.
//!
//! Attribution is best-effort by construction — the holder of a lease
//! overlapping the file, else "external" (the user's editor, `git
//! checkout`, a build) — and every rendering says so. Paths are relative to
//! the checkout, so the same file is the same entry from a worktree or a
//! container mount and "everything under `src/`" is a prefix.

use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AgentId, LeaseId, ProjectId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Created,
    Modified,
    Removed,
    Renamed,
}

impl fmt::Display for ChangeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Created => "created",
            Self::Modified => "modified",
            Self::Removed => "removed",
            Self::Renamed => "renamed",
        })
    }
}

/// Who made a change, as far as the daemon can tell.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum Attribution {
    /// An agent held a lease on the file when it changed.
    Agent {
        agent: AgentId,
        lease: LeaseId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// Nobody held the file: an editor, a git command, a build.
    External,
}

impl Attribution {
    pub fn agent(&self) -> Option<&AgentId> {
        match self {
            Self::Agent { agent, .. } => Some(agent),
            Self::External => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// Position in the ledger, assigned by the store; `0` until stored.
    #[serde(default)]
    pub seq: u64,
    pub project: ProjectId,
    /// Physical checkout identity; absent only in legacy ledger entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout: Option<PathBuf>,
    /// The linked worktree the change happened in, if not the main checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PathBuf>,
    /// Relative to the checkout.
    pub path: PathBuf,
    pub kind: ChangeKind,
    pub at: DateTime<Utc>,
    #[serde(flatten)]
    pub by: Attribution,
    /// The checkout's HEAD when the change was seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_flat_and_round_trips() {
        let change = Change {
            seq: 7,
            project: ProjectId::from("abc"),
            checkout: None,
            worktree: None,
            path: PathBuf::from("src/lib.rs"),
            kind: ChangeKind::Modified,
            at: Utc::now(),
            by: Attribution::Agent {
                agent: AgentId::from("a1"),
                lease: LeaseId::from("l1"),
                note: Some("refactoring".into()),
            },
            head: Some("3f9c1e0".into()),
        };
        let json = serde_json::to_value(&change).unwrap();
        assert_eq!(json["by"], "agent");
        assert_eq!(json["agent"], "a1");
        assert_eq!(json["kind"], "modified");
        assert_eq!(serde_json::from_value::<Change>(json).unwrap(), change);

        let external = Change {
            by: Attribution::External,
            ..change
        };
        let json = serde_json::to_value(&external).unwrap();
        assert_eq!(json["by"], "external");
        assert!(json.get("agent").is_none());
        assert_eq!(external.by.agent(), None);
    }
}
