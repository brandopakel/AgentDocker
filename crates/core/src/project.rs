//! Projects: where an agent works, derived from its working directory.
//!
//! A project is never declared. The daemon derives it from an agent's
//! `workdir` (the discovery itself is I/O and lives in `agentdocker-host`):
//! the enclosing git repository — every worktree of one repository is the
//! same project — else the nearest `Agentfile.toml`, else the directory
//! itself. This module holds the pure half: the reference and its id.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How a project root was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectSource {
    /// The root holds the repository's `.git`.
    Git,
    /// The root holds an `Agentfile.toml`.
    Agentfile,
    /// Neither was found; the working directory is its own project.
    Directory,
}

/// A project as seen from one agent's working directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRef {
    /// Canonical root on this host — the main repository for git projects,
    /// even when the agent sits in a linked worktree.
    pub root: PathBuf,
    /// The linked worktree the agent works in, if not the main one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PathBuf>,
    /// The repository's oldest root commit. Identifies the same repository
    /// across clones and, later, across hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    pub source: ProjectSource,
}

impl ProjectRef {
    /// A directory that is its own project.
    pub fn directory(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            worktree: None,
            fingerprint: None,
            source: ProjectSource::Directory,
        }
    }

    /// The fingerprint when there is one, else a stable hash of the root
    /// path, so two agents in one directory always share an id even
    /// without git.
    pub fn id(&self) -> ProjectId {
        match &self.fingerprint {
            Some(fingerprint) => ProjectId(fingerprint.clone()),
            None => ProjectId(
                uuid::Uuid::new_v5(
                    &uuid::Uuid::NAMESPACE_URL,
                    self.root.to_string_lossy().as_bytes(),
                )
                .simple()
                .to_string(),
            ),
        }
    }

    /// The root's last path component, for tables and headings.
    pub fn name(&self) -> String {
        self.root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.to_string_lossy().into_owned())
    }

    /// Where this agent's files actually are: the worktree, else the root.
    pub fn dir(&self) -> &Path {
        self.worktree.as_deref().unwrap_or(&self.root)
    }
}

/// Identifies a project independently of where it is checked out. Like an
/// [`crate::AgentId`], any unique prefix works on the command line.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectId(String);

impl ProjectId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Abbreviated form for tables, matching [`crate::AgentId::short`].
    pub fn short(&self) -> &str {
        let end = self
            .0
            .char_indices()
            .nth(12)
            .map_or(self.0.len(), |(i, _)| i);
        &self.0[..end]
    }
}

impl From<&str> for ProjectId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_the_id_when_present() {
        let mut project = ProjectRef::directory("/repo");
        project.source = ProjectSource::Git;
        project.fingerprint = Some("abc123".to_owned());
        assert_eq!(project.id().as_str(), "abc123");
    }

    #[test]
    fn path_hash_is_stable_and_distinct() {
        let a = ProjectRef::directory("/repo/a");
        let b = ProjectRef::directory("/repo/b");
        assert_eq!(a.id(), ProjectRef::directory("/repo/a").id());
        assert_ne!(a.id(), b.id());
        assert_eq!(a.id().as_str().len(), 32);
        assert_eq!(a.id().short().len(), 12);
    }

    #[test]
    fn name_and_dir_prefer_the_worktree_for_files_only() {
        let mut project = ProjectRef::directory("/home/me/repo");
        project.worktree = Some(PathBuf::from("/home/me/repo-wt"));
        assert_eq!(project.name(), "repo");
        assert_eq!(project.dir(), Path::new("/home/me/repo-wt"));
        project.worktree = None;
        assert_eq!(project.dir(), Path::new("/home/me/repo"));
    }

    #[test]
    fn serialises_compactly() {
        let project = ProjectRef::directory("/repo");
        let json = serde_json::to_string(&project).unwrap();
        assert_eq!(json, r#"{"root":"/repo","source":"directory"}"#);
        assert_eq!(serde_json::from_str::<ProjectRef>(&json).unwrap(), project);
    }
}
