//! What an agent changed on its branch, against the branch it would be
//! merged into, and whether a merge can be made now. The daemon reads git;
//! everything here is the shape of the answer and the rules over it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::AgentId;

/// At most this many bytes of patch are sent; the file list is complete.
pub const PATCH_LIMIT: usize = 512 * 1024;
/// At most this many commits and files are listed.
pub const LIST_LIMIT: usize = 200;

/// An agent's branch and what merging it would bring.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub agent: AgentId,
    /// The agent's checkout and the branch on it.
    pub checkout: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// The project's main checkout and the branch on it: where a merge
    /// lands.
    pub target_checkout: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Where the agent's branch split from the target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_base: Option<String>,
    /// The agent's commits not on the target, newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<Commit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<File>,
    /// The unified diff from the merge base to the agent's head.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub patch: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Tracked files the agent changed and did not commit: not part of a
    /// merge.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncommitted: Vec<String>,
    /// Tracked files changed in the main checkout: a merge waits for them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_dirty: Vec<String>,
    /// Files a merge would conflict in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<String>,
    /// Paths in the main checkout someone holds a lease on, with who.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<(String, String)>,
}

impl Default for Review {
    fn default() -> Self {
        Self {
            agent: AgentId::from(String::new()),
            checkout: PathBuf::new(),
            branch: None,
            head: None,
            target_checkout: PathBuf::new(),
            target: None,
            merge_base: None,
            commits: Vec::new(),
            files: Vec::new(),
            patch: String::new(),
            truncated: false,
            uncommitted: Vec::new(),
            target_dirty: Vec::new(),
            conflicts: Vec::new(),
            held: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub sha: String,
    pub subject: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct File {
    pub path: String,
    /// `A`dded, `M`odified, `D`eleted, `R`enamed, ...
    pub status: String,
    /// Lines; none for a binary file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed: Option<u64>,
}

/// Why a merge cannot be made now, in the order the person meets them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocker {
    /// The agent works in the main checkout on the target branch: its
    /// changes are there already.
    SameCheckout,
    /// The main checkout has no branch checked out, or the agent's
    /// checkout has none.
    NoBranch,
    /// The agent's branch has nothing the target does not.
    NothingToMerge,
    Uncommitted(usize),
    TargetDirty(usize),
    Conflicts(usize),
    Held(usize),
}

impl Blocker {
    /// What the checklist says, in the person's words.
    pub fn label(&self, target: &str) -> String {
        let files = |n: &usize| {
            if *n == 1 {
                "1 file".to_owned()
            } else {
                format!("{n} files")
            }
        };
        match self {
            Self::SameCheckout => {
                format!(
                    "works directly on {target} in the main folder; its changes are there already"
                )
            }
            Self::NoBranch => "no branch is checked out to merge into or from".to_owned(),
            Self::NothingToMerge => format!("nothing on its branch that {target} does not have"),
            Self::Uncommitted(n) => format!("{} not committed yet", files(n)),
            Self::TargetDirty(n) => format!("{target} has {} changed and not committed", files(n)),
            Self::Conflicts(n) => format!("would conflict in {}", files(n)),
            Self::Held(n) => format!("{} in the main folder held by another agent", files(n)),
        }
    }
}

impl Review {
    /// Everything that stops a merge now; none means it can be made.
    pub fn blockers(&self) -> Vec<Blocker> {
        let mut blockers = Vec::new();
        if self.checkout == self.target_checkout && self.branch == self.target {
            return vec![Blocker::SameCheckout];
        }
        if self.branch.is_none() || self.target.is_none() {
            return vec![Blocker::NoBranch];
        }
        if self.commits.is_empty() {
            blockers.push(Blocker::NothingToMerge);
        }
        if !self.uncommitted.is_empty() {
            blockers.push(Blocker::Uncommitted(self.uncommitted.len()));
        }
        if !self.target_dirty.is_empty() {
            blockers.push(Blocker::TargetDirty(self.target_dirty.len()));
        }
        if !self.conflicts.is_empty() {
            blockers.push(Blocker::Conflicts(self.conflicts.len()));
        }
        if !self.held.is_empty() {
            blockers.push(Blocker::Held(self.held.len()));
        }
        blockers
    }

    /// Lines added and removed across every file that says.
    pub fn totals(&self) -> (u64, u64) {
        self.files.iter().fold((0, 0), |(a, r), f| {
            (a + f.added.unwrap_or(0), r + f.removed.unwrap_or(0))
        })
    }
}

/// `git log --format=%H%x1f%s` lines.
pub fn parse_log(text: &str) -> Vec<Commit> {
    text.lines()
        .filter_map(|line| {
            let (sha, subject) = line.split_once('\u{1f}')?;
            Some(Commit {
                sha: sha.trim().to_owned(),
                subject: subject.to_owned(),
            })
        })
        .take(LIST_LIMIT)
        .collect()
}

/// `git diff --name-status` and `git diff --numstat` of one range, joined
/// by path. A rename reads as its new path.
pub fn parse_files(name_status: &str, numstat: &str) -> Vec<File> {
    let counts: std::collections::HashMap<&str, (Option<u64>, Option<u64>)> = numstat
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let added = parts.next()?.parse().ok();
            let removed = parts.next()?.parse().ok();
            let path = parts.next()?;
            // A rename is `old => new` or `dir/{old => new}/x`; the new path
            // is what name-status names last.
            Some((renamed(path), (added, removed)))
        })
        .collect();
    name_status
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let status = fields.first()?.chars().next()?.to_string();
            let path = *fields.last()?;
            let (added, removed) = counts.get(path).copied().unwrap_or((None, None));
            Some(File {
                path: path.to_owned(),
                status,
                added,
                removed,
            })
        })
        .take(LIST_LIMIT)
        .collect()
}

fn renamed(path: &str) -> &str {
    match path.rsplit_once(" => ") {
        Some((_, new)) if !path.contains('{') => new,
        _ => path,
    }
}

/// Tracked paths `git status --porcelain` reports changed; untracked
/// files (`??`) are not part of a commit or a merge, and are left out.
pub fn parse_porcelain(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| line.len() > 3 && !line.starts_with("??") && !line.starts_with("!!"))
        .map(|line| line[3..].to_owned())
        .take(LIST_LIMIT)
        .collect()
}

/// The conflicted paths from `git merge-tree --write-tree --name-only`
/// when it exits 1: the tree id on the first line, then the paths up to
/// the blank line before its messages.
pub fn parse_conflicts(text: &str) -> Vec<String> {
    text.lines()
        .skip(1)
        .take_while(|line| !line.is_empty())
        .map(str::to_owned)
        .take(LIST_LIMIT)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_output_reads_into_commits_files_changes_and_conflicts() {
        let commits = parse_log("abc123\u{1f}Add login\ndef456\u{1f}Fix tests\n");
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].subject, "Add login");

        let files = parse_files(
            "M\tsrc/auth.rs\nA\tsrc/session.rs\nR100\told.rs\tnew.rs\nM\tlogo.png\n",
            "80\t12\tsrc/auth.rs\n35\t0\tsrc/session.rs\n0\t0\told.rs => new.rs\n-\t-\tlogo.png\n",
        );
        assert_eq!(
            files
                .iter()
                .map(|f| (f.path.as_str(), f.status.as_str(), f.added, f.removed))
                .collect::<Vec<_>>(),
            [
                ("src/auth.rs", "M", Some(80), Some(12)),
                ("src/session.rs", "A", Some(35), Some(0)),
                ("new.rs", "R", Some(0), Some(0)),
                ("logo.png", "M", None, None),
            ]
        );

        assert_eq!(
            parse_porcelain(" M src/a.rs\nM  src/b.rs\n?? scratch.txt\n"),
            ["src/a.rs", "src/b.rs"]
        );
        assert_eq!(
            parse_conflicts(
                "4b825dc6\nsrc/a.rs\nsrc/b.rs\n\nAuto-merging src/a.rs\nCONFLICT (content)\n"
            ),
            ["src/a.rs", "src/b.rs"]
        );
    }

    #[test]
    fn a_merge_waits_for_every_blocker_and_says_each_one() {
        let ready = Review {
            checkout: "/wt/login".into(),
            branch: Some("feat/login".into()),
            target_checkout: "/repo".into(),
            target: Some("main".into()),
            commits: vec![Commit {
                sha: "abc".into(),
                subject: "Add login".into(),
            }],
            ..Review::default()
        };
        assert!(ready.blockers().is_empty());

        let mut blocked = ready.clone();
        blocked.uncommitted = vec!["src/a.rs".into()];
        blocked.target_dirty = vec!["CLAUDE.md".into(), "x".into()];
        blocked.conflicts = vec!["src/b.rs".into()];
        assert_eq!(
            blocked.blockers(),
            [
                Blocker::Uncommitted(1),
                Blocker::TargetDirty(2),
                Blocker::Conflicts(1)
            ]
        );
        assert_eq!(
            Blocker::TargetDirty(2).label("main"),
            "main has 2 files changed and not committed"
        );

        let mut same = ready.clone();
        same.checkout = "/repo".into();
        same.branch = Some("main".into());
        assert_eq!(same.blockers(), [Blocker::SameCheckout]);

        let mut empty = ready;
        empty.commits.clear();
        assert_eq!(empty.blockers(), [Blocker::NothingToMerge]);
    }
}
