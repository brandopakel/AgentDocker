//! The attribution ledger: one entry per file change the project watcher
//! saw, with whoever held the file at the time.
//!
//! Attribution is best-effort by construction — the holder of a lease
//! overlapping the file, else "external" (the user's editor, `git
//! checkout`, a build) — and every rendering says so. Paths are relative to
//! the checkout, so the same file is the same entry from a worktree or a
//! container mount and "everything under `src/`" is a prefix.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

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

/// One checkout's share of a path that more than one checkout changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlapParty {
    pub checkout: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PathBuf>,
    /// Agents attributed with changes there, in order of first change;
    /// empty when every change was external.
    #[serde(default)]
    pub agents: Vec<AgentId>,
    pub changes: usize,
    pub last_at: DateTime<Utc>,
    /// The checkout's HEAD at its last change there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
}

/// A project-relative path changed in more than one physical checkout:
/// what will collide when the branches meet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overlap {
    pub path: PathBuf,
    /// Newest change first.
    pub parties: Vec<OverlapParty>,
}

/// Group ledger rows by path and by the checkout they happened in, and
/// keep the paths that more than one checkout touched. Rows that do not
/// say which checkout they belong to are ignored. Paths come sorted.
pub fn overlaps(changes: &[Change]) -> Vec<Overlap> {
    let mut by_path: BTreeMap<&Path, BTreeMap<&Path, OverlapParty>> = BTreeMap::new();
    for change in changes {
        let Some(checkout) = change.checkout.as_deref() else {
            continue;
        };
        let party = by_path
            .entry(change.path.as_path())
            .or_default()
            .entry(checkout)
            .or_insert_with(|| OverlapParty {
                checkout: checkout.to_path_buf(),
                worktree: change.worktree.clone(),
                agents: Vec::new(),
                changes: 0,
                last_at: change.at,
                head: change.head.clone(),
            });
        party.changes += 1;
        if change.at >= party.last_at {
            party.last_at = change.at;
            party.head = change.head.clone();
        }
        if let Some(agent) = change.by.agent() {
            if !party.agents.contains(agent) {
                party.agents.push(agent.clone());
            }
        }
    }
    by_path
        .into_iter()
        .filter(|(_, parties)| parties.len() > 1)
        .map(|(path, parties)| {
            let mut parties: Vec<OverlapParty> = parties.into_values().collect();
            parties.sort_by(|a, b| {
                b.last_at
                    .cmp(&a.last_at)
                    .then_with(|| a.checkout.cmp(&b.checkout))
            });
            Overlap {
                path: path.to_path_buf(),
                parties,
            }
        })
        .collect()
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

    #[test]
    fn overlaps_are_paths_changed_in_more_than_one_checkout() {
        let now = Utc::now();
        let change =
            |checkout: Option<&str>, path: &str, agent: Option<&str>, minutes: i64| Change {
                seq: 0,
                project: ProjectId::from("p"),
                checkout: checkout.map(PathBuf::from),
                worktree: checkout.filter(|c| c.ends_with("wt")).map(PathBuf::from),
                path: PathBuf::from(path),
                kind: ChangeKind::Modified,
                at: now - chrono::Duration::minutes(minutes),
                by: match agent {
                    Some(a) => Attribution::Agent {
                        agent: AgentId::from(a),
                        lease: LeaseId::from("l"),
                        note: None,
                    },
                    None => Attribution::External,
                },
                head: Some(format!("h{minutes}")),
            };
        let rows = vec![
            change(Some("/repo"), "src/a.rs", Some("a1"), 10),
            change(Some("/repo"), "src/a.rs", Some("a1"), 9),
            change(Some("/repo"), "src/a.rs", None, 8),
            change(Some("/wt"), "src/a.rs", Some("b2"), 2),
            change(Some("/wt"), "src/only.rs", Some("b2"), 1),
            change(Some("/repo"), "docs/x.md", None, 5),
            change(None, "docs/x.md", Some("c3"), 4),
        ];
        let found = overlaps(&rows);
        assert_eq!(found.len(), 1, "{found:?}");
        let overlap = &found[0];
        assert_eq!(overlap.path, PathBuf::from("src/a.rs"));
        assert_eq!(overlap.parties.len(), 2);
        assert_eq!(
            overlap.parties[0].checkout,
            PathBuf::from("/wt"),
            "newest first"
        );
        assert_eq!(overlap.parties[0].worktree, Some(PathBuf::from("/wt")));
        assert_eq!(overlap.parties[0].agents, vec![AgentId::from("b2")]);
        assert_eq!(overlap.parties[0].changes, 1);
        assert_eq!(overlap.parties[1].checkout, PathBuf::from("/repo"));
        assert_eq!(
            overlap.parties[1].agents,
            vec![AgentId::from("a1")],
            "deduplicated, external skipped"
        );
        assert_eq!(overlap.parties[1].changes, 3);
        assert_eq!(
            overlap.parties[1].head.as_deref(),
            Some("h8"),
            "head at the last change"
        );
        assert!(
            overlaps(&rows[..3]).is_empty(),
            "one checkout is no overlap"
        );
    }
}
