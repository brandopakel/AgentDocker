//! Branch and head of a checkout, read from `.git` directly.
//!
//! Two small files answer "which branch, which commit" in microseconds
//! with no `git` process, so this can run on every hook fire and every
//! daemon tick. Linked worktrees keep `HEAD` in their own git directory
//! and share refs through the common one; packed refs are consulted when a
//! loose ref file is missing. Dirtiness is not computed here — it needs
//! the index and a walk — and stays `None` until something cheap exists.

#[cfg(test)]
use std::fs;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use agentdocker_core::VcsState;
use chrono::Utc;

use crate::project::resolve;

/// The checkout containing `dir`, or `None` outside any repository.
pub fn state(dir: &Path) -> Option<VcsState> {
    let observed_at = Utc::now();
    let (gitdir, common) = git_dirs(dir)?;
    let head = read_metadata(&gitdir.join("HEAD"), 4096)?;
    let head = head.trim();
    let (branch, sha) = match head.strip_prefix("ref:") {
        Some(refname) => {
            let refname = refname.trim();
            if !valid_ref(refname) {
                return None;
            }
            let branch = refname
                .strip_prefix("refs/heads/")
                .unwrap_or(refname)
                .to_owned();
            (Some(branch), resolve_ref(&common, refname))
        }
        None if head.is_empty() => return None,
        None if valid_oid(head) => (None, Some(head.to_owned())),
        None => return None,
    };
    Some(VcsState {
        branch,
        head: sha,
        dirty: None,
        updated_at: observed_at,
    })
}

/// The subject line of a commit, via `git log`; `None` when git is
/// missing, the object is unknown, or `timeout` passes.
pub fn subject(dir: &Path, sha: &str, timeout: std::time::Duration) -> Option<String> {
    if sha.is_empty() || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut child = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["log", "-1", "--format=%s", sha, "--"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    let subject = out.trim();
    (!subject.is_empty()).then(|| subject.chars().take(200).collect())
}

/// `(git directory of this checkout, common git directory)`: equal for a
/// main checkout, distinct for a linked worktree.
pub fn git_dirs(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let start = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    for ancestor in start.ancestors() {
        let dot_git = ancestor.join(".git");
        if dot_git.is_dir() {
            return Some((dot_git.clone(), dot_git));
        }
        if dot_git.is_file() {
            let text = read_metadata(&dot_git, 4096)?;
            let gitdir = resolve(ancestor, text.trim().strip_prefix("gitdir:")?.trim());
            let common = match read_metadata(&gitdir.join("commondir"), 4096) {
                Some(common) => resolve(&gitdir, common.trim()),
                None => gitdir.clone(),
            };
            return Some((gitdir, common));
        }
    }
    None
}

/// Read only bounded regular files. O_NONBLOCK also prevents FIFO open from hanging.
pub(crate) fn read_metadata(path: &Path, limit: u64) -> Option<String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > limit {
        return None;
    }
    let mut text = String::new();
    file.take(limit + 1).read_to_string(&mut text).ok()?;
    (text.len() as u64 <= limit).then_some(text)
}

/// Ref names must stay below refs/, without traversal or terminal control characters.
fn valid_ref(name: &str) -> bool {
    name.starts_with("refs/")
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && !name.chars().any(|c| c.is_control() || c == '\\')
}

/// Git repositories currently use SHA-1 or SHA-256 object identifiers.
fn valid_oid(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn resolve_ref(common: &Path, refname: &str) -> Option<String> {
    if !valid_ref(refname) {
        return None;
    }
    let root = common.canonicalize().ok()?;
    if let Ok(path) = root.join(refname).canonicalize() {
        if !path.starts_with(&root) {
            return None;
        }
        if let Some(loose) = read_metadata(&path, 4096) {
            let sha = loose.trim();
            if valid_oid(sha) {
                return Some(sha.to_owned());
            }
        }
    }
    let packed = read_metadata(&root.join("packed-refs"), 4 * 1024 * 1024)?;
    packed
        .lines()
        .filter(|line| !line.starts_with('#') && !line.starts_with('^'))
        .find_map(|line| {
            let (sha, name) = line.split_once(' ')?;
            (name.trim() == refname && valid_oid(sha)).then(|| sha.to_owned())
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

    #[test]
    fn unsafe_or_unbounded_metadata_is_not_read() {
        let tmp = TempDir::new().unwrap();
        let dot = tmp.path().join(".git");
        fs::create_dir(&dot).unwrap();
        for name in ["/etc/passwd", "refs/../../secret", "refs/heads/x\nINJECT"] {
            fs::write(dot.join("HEAD"), format!("ref: {name}")).unwrap();
            assert_eq!(state(tmp.path()), None);
        }
        fs::write(dot.join("HEAD"), "x".repeat(8192)).unwrap();
        assert_eq!(state(tmp.path()), None);
        fs::remove_file(dot.join("HEAD")).unwrap();
        let name = std::ffi::CString::new(dot.join("HEAD").to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert_eq!(state(tmp.path()), None);
    }

    #[test]
    fn loose_refs_cannot_escape_through_symlinks() {
        let tmp = TempDir::new().unwrap();
        let dot = tmp.path().join(".git");
        fs::create_dir_all(dot.join("refs/heads")).unwrap();
        fs::write(dot.join("HEAD"), "ref: refs/heads/main").unwrap();
        let secret = tmp.path().join("secret");
        fs::write(&secret, "a".repeat(40)).unwrap();
        std::os::unix::fs::symlink(secret, dot.join("refs/heads/main")).unwrap();
        assert_eq!(state(tmp.path()).unwrap().head, None);
    }
}
