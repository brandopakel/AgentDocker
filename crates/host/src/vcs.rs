//! Branch and head of a checkout, read from `.git` directly.
//!
//! Two small files answer "which branch, which commit" in microseconds
//! with no `git` process, so this can run on every hook fire and every
//! daemon tick. Linked worktrees keep `HEAD` in their own git directory
//! and share refs through the common one; packed refs are consulted when a
//! loose ref file is missing. Dirtiness is not computed here — it needs
//! the index and a walk — and stays `None` until something cheap exists.

use std::fs;
use std::path::{Path, PathBuf};

use agentdocker_core::VcsState;
use chrono::Utc;

use crate::project::resolve;

/// The checkout containing `dir`, or `None` outside any repository.
pub fn state(dir: &Path) -> Option<VcsState> {
    let (gitdir, common) = git_dirs(dir)?;
    let head = fs::read_to_string(gitdir.join("HEAD")).ok()?;
    let head = head.trim();
    let (branch, sha) = match head.strip_prefix("ref:") {
        Some(refname) => {
            let refname = refname.trim();
            let branch = refname
                .strip_prefix("refs/heads/")
                .unwrap_or(refname)
                .to_owned();
            (Some(branch), resolve_ref(&common, refname))
        }
        None if head.is_empty() => return None,
        None => (None, Some(head.to_owned())),
    };
    Some(VcsState {
        branch,
        head: sha,
        dirty: None,
        updated_at: Utc::now(),
    })
}

/// `(git directory of this checkout, common git directory)`: equal for a
/// main checkout, distinct for a linked worktree.
fn git_dirs(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let start = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    for ancestor in start.ancestors() {
        let dot_git = ancestor.join(".git");
        if dot_git.is_dir() {
            return Some((dot_git.clone(), dot_git));
        }
        if dot_git.is_file() {
            let text = fs::read_to_string(&dot_git).ok()?;
            let gitdir = resolve(ancestor, text.trim().strip_prefix("gitdir:")?.trim());
            let common = match fs::read_to_string(gitdir.join("commondir")) {
                Ok(common) => resolve(&gitdir, common.trim()),
                Err(_) => gitdir.clone(),
            };
            return Some((gitdir, common));
        }
    }
    None
}

fn resolve_ref(common: &Path, refname: &str) -> Option<String> {
    if let Ok(loose) = fs::read_to_string(common.join(refname)) {
        let sha = loose.trim();
        if !sha.is_empty() && !sha.starts_with("ref:") {
            return Some(sha.to_owned());
        }
    }
    let packed = fs::read_to_string(common.join("packed-refs")).ok()?;
    packed
        .lines()
        .filter(|line| !line.starts_with('#') && !line.starts_with('^'))
        .find_map(|line| {
            let (sha, name) = line.split_once(' ')?;
            (name.trim() == refname).then(|| sha.to_owned())
        })
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    use tempfile::TempDir;

    use super::*;

    fn git(home: &Path, dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stderr(Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn have_git() -> bool {
        Command::new("git")
            .arg("--version")
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn repo(tmp: &TempDir) -> PathBuf {
        let main = tmp.path().join("main");
        fs::create_dir_all(&main).unwrap();
        git(tmp.path(), &main, &["init", "-q"]);
        git(
            tmp.path(),
            &main,
            &["commit", "-q", "--allow-empty", "-m", "root"],
        );
        main
    }

    #[test]
    fn branch_and_head_from_a_nested_directory() {
        if !have_git() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let main = repo(&tmp);
        let head = git(tmp.path(), &main, &["rev-parse", "HEAD"]);
        let nested = main.join("src/deep");
        fs::create_dir_all(&nested).unwrap();
        let vcs = state(&nested).expect("inside a repo");
        assert_eq!(vcs.branch.as_deref(), Some("main"));
        assert_eq!(vcs.head.as_deref(), Some(head.as_str()));
        assert_eq!(vcs.dirty, None);
        assert_eq!(vcs.describe(), format!("main@{}", &head[..7]));
    }

    #[test]
    fn detached_head_and_packed_refs() {
        if !have_git() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let main = repo(&tmp);
        let head = git(tmp.path(), &main, &["rev-parse", "HEAD"]);
        git(tmp.path(), &main, &["checkout", "-q", "--detach"]);
        let vcs = state(&main).unwrap();
        assert_eq!(vcs.branch, None);
        assert_eq!(vcs.head.as_deref(), Some(head.as_str()));
        assert_eq!(vcs.describe(), format!("(detached)@{}", &head[..7]));

        git(tmp.path(), &main, &["checkout", "-q", "main"]);
        git(tmp.path(), &main, &["pack-refs", "--all"]);
        assert!(!main.join(".git/refs/heads/main").exists(), "ref is packed");
        let vcs = state(&main).unwrap();
        assert_eq!(vcs.branch.as_deref(), Some("main"));
        assert_eq!(vcs.head.as_deref(), Some(head.as_str()));
    }

    #[test]
    fn linked_worktree_has_its_own_branch() {
        if !have_git() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let main = repo(&tmp);
        let wt = tmp.path().join("wt");
        git(
            tmp.path(),
            &main,
            &[
                "worktree",
                "add",
                "-q",
                wt.to_str().unwrap(),
                "-b",
                "feature",
            ],
        );
        assert_eq!(state(&wt).unwrap().branch.as_deref(), Some("feature"));
        assert_eq!(state(&main).unwrap().branch.as_deref(), Some("main"));
        assert_eq!(state(&wt).unwrap().head, state(&main).unwrap().head);
    }

    #[test]
    fn unborn_branch_and_no_repository() {
        if !have_git() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let empty = tmp.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        git(tmp.path(), &empty, &["init", "-q"]);
        let vcs = state(&empty).unwrap();
        assert_eq!(vcs.branch.as_deref(), Some("main"));
        assert_eq!(vcs.head, None);
        assert_eq!(vcs.describe(), "main (unborn)");
        assert_eq!(state(tmp.path()), None);
    }
}
