//! Persisted observations belong to physical paths, independently of logical projects.
use crate::{AgentId, Change};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A content version observed before an agent reads a file or searches a directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadMark {
    pub path: PathBuf,
    pub at: DateTime<Utc>,
    pub version: String,
    pub head: Option<String>,
}

/// A retained observation whose content no longer matches the physical checkout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StalePath {
    pub path: PathBuf,
    pub observed: String,
    pub current: Option<String>,
    pub reason: String,
}

/// Whether retained ledger rows prevent an independently content-proven own
/// change from clearing a read guard. Paths are relative to the checkout;
/// `complete` refers only to the returned page, never to OS event delivery.
///
/// The caller must first compare the target's current identity with its read
/// baseline and matching attributed write evidence. An old own/sibling ledger
/// row alone cannot explain a newer fingerprint. Empty or partial pages refuse
/// the exemption; so does a retained external change to the target. Watcher
/// coverage, application queue draining and content proofs are caller concerns.
pub fn stale_by_ledger(
    agent: &AgentId,
    mark: &Path,
    target: &Path,
    changes: &[Change],
    complete: bool,
) -> bool {
    if changes.is_empty() || !complete {
        return true;
    }
    changes
        .iter()
        .filter(|change| target == mark || change.path.starts_with(target))
        .any(|change| change.by.agent() != Some(agent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::{Attribution, ChangeKind};

    fn change(path: &str, by: Option<&str>) -> Change {
        Change {
            seq: 0,
            project: crate::ProjectId::from("p"),
            checkout: None,
            worktree: None,
            path: PathBuf::from(path),
            kind: ChangeKind::Modified,
            at: Utc::now(),
            by: match by {
                Some(agent) => Attribution::Agent {
                    agent: AgentId::from(agent),
                    lease: crate::LeaseId::from("l"),
                    note: None,
                },
                None => Attribution::External,
            },
            head: None,
        }
    }

    /// A directory observation is stale for a target only when the ledger
    /// says the target itself changed, by someone other than the asker; an
    /// unexplained difference stays stale, and so does one the ledger only
    /// partly answered.
    #[test]
    fn a_directory_observation_is_stale_only_for_what_someone_else_changed() {
        let me = AgentId::from("me");
        let dir = Path::new("repro");
        let target = Path::new("repro/e2e/main.go");
        let elsewhere = [change("repro/old/notes.md", None)];
        assert!(!stale_by_ledger(&me, dir, target, &elsewhere, true));
        assert!(
            stale_by_ledger(&me, dir, target, &elsewhere, false),
            "partial"
        );
        assert!(stale_by_ledger(&me, dir, target, &[], true), "unexplained");
        let mine = [change("repro/e2e/main.go", Some("me"))];
        assert!(
            !stale_by_ledger(&me, dir, target, &mine, true),
            "my own edit"
        );
        assert!(
            stale_by_ledger(&me, dir, target, &mine, false),
            "partial, showing only my own edit"
        );
        let theirs = [
            change("repro/e2e/main.go", Some("me")),
            change("repro/e2e/main.go", Some("peer")),
        ];
        assert!(stale_by_ledger(&me, dir, target, &theirs, true));
        let outside = [change("repro/e2e/main.go", None)];
        assert!(
            stale_by_ledger(&me, dir, target, &outside, true),
            "external"
        );
        // A file observation counts every change to it; a directory target
        // counts every change beneath it.
        let file = Path::new("src/lib.rs");
        assert!(!stale_by_ledger(
            &me,
            file,
            file,
            &[change("src/lib.rs", Some("me"))],
            true
        ));
        assert!(stale_by_ledger(
            &me,
            file,
            file,
            &[change("src/lib.rs", None)],
            true
        ));
        assert!(stale_by_ledger(
            &me,
            dir,
            Path::new("repro/e2e"),
            &[change("repro/e2e/main.go", Some("peer"))],
            true
        ));
    }
}
