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
use std::io::Read;
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

/// Reflog entries read at most, newest first: far more than a checkout
/// moves between two looks.
const REFLOG_ENTRIES: &str = "64";

/// Whether git's own record of this checkout's HEAD says a commit was made
/// here, on the branch it is on, since HEAD was last seen at `before`. Two
/// heads cannot say it: a fast-forward of one commit fetched from elsewhere
/// has the old head as its parent just as a commit made here does. The
/// checkout's HEAD reflog, which git keeps per worktree, names the
/// operation that made each move. See [`committed_between`]. `false` when
/// git cannot answer within `timeout`.
pub fn committed_here(dir: &Path, before: &str, after: &str, timeout: std::time::Duration) -> bool {
    if !valid_oid(before) || !valid_oid(after) || before == after {
        return false;
    }
    let argv: Vec<String> = [
        "git",
        "reflog",
        "show",
        "--no-show-signature",
        "--format=%H%x09%gs",
        "-n",
        REFLOG_ENTRIES,
        "HEAD",
        "--",
    ]
    .map(str::to_owned)
    .to_vec();
    let quiet = [("GIT_OPTIONAL_LOCKS", Some(std::ffi::OsStr::new("0")))];
    match crate::command::run_with_env(dir, &argv, timeout, &quiet) {
        Ok(output) if output.success => committed_between(&output.stdout, before, after),
        _ => false,
    }
}

/// Whether a HEAD reflog (`<commit>\t<subject>` a line, newest first) shows
/// a commit made with `git commit` — its entries read `commit: …`,
/// `commit (amend): …`, `commit (merge): …` — between the entry that moved
/// HEAD to `after` and the one that moved it to `before`, with no branch
/// switch (`checkout: …`) after the commit. A pull, a merge git concludes
/// by itself, a reset, a cherry-pick or a rebase is not counted. A reflog
/// that never reaches `after`, or ends before `before` or a switch, says
/// no: it cannot tell what happened.
pub fn committed_between(reflog: &str, before: &str, after: &str) -> bool {
    let entries = reflog
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .skip_while(|(commit, _)| *commit != after);
    let mut made = false;
    for (index, (commit, subject)) in entries.enumerate() {
        // The entry that moved HEAD to `after` is the newest one read; the
        // first older one to land on `before` is where the last look was.
        if (index > 0 && commit == before) || subject.starts_with("checkout:") {
            return made;
        }
        made |= subject.starts_with("commit:") || subject.starts_with("commit (");
    }
    false
}

/// Every checkout of the repository `dir` belongs to: the main one and
/// each linked worktree, canonical, in the order git lists them.
///
/// A project is the repository, not one directory of it — `overlap`
/// exists precisely to compare what two checkouts of it have changed —
/// so anything that observes a project has to know where its checkouts
/// are. `git worktree list --porcelain` is the only authority on that:
/// the linked ones can be anywhere on the disk, and the daemon has no
/// other way to find one nobody told it about.
///
/// Bounded and best-effort. An empty answer means "ask again later",
/// never "there is only this one" — a caller that treats it as the
/// latter would silently stop watching real checkouts.
pub fn worktrees(dir: &Path, timeout: std::time::Duration) -> Vec<PathBuf> {
    let Ok(mut child) = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["worktree", "list", "--porcelain"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => {
                let _ = child.wait();
                return Vec::new();
            }
            Ok(None) => {}
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Vec::new();
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let Some(mut stdout) = child.stdout.take() else {
        return Vec::new();
    };
    let mut out = String::new();
    if std::io::Read::read_to_string(&mut stdout, &mut out).is_err() {
        return Vec::new();
    }
    out.lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(|path| {
            let path = PathBuf::from(path);
            path.canonicalize().unwrap_or(path)
        })
        .collect()
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
    let file = crate::files::open_regular(path).ok()?;
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
        #[cfg(unix)]
        {
            let name = std::ffi::CString::new(dot.join("HEAD").to_str().unwrap()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            assert_eq!(state(tmp.path()), None);
        }
    }

    #[test]
    fn loose_refs_cannot_escape_through_symlinks() {
        let tmp = TempDir::new().unwrap();
        let dot = tmp.path().join(".git");
        fs::create_dir_all(dot.join("refs/heads")).unwrap();
        fs::write(dot.join("HEAD"), "ref: refs/heads/main").unwrap();
        let secret = tmp.path().join("secret");
        fs::write(&secret, "a".repeat(40)).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(secret, dot.join("refs/heads/main")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(secret, dot.join("refs/heads/main")).unwrap();
        assert_eq!(state(tmp.path()).unwrap().head, None);
    }

    /// Only a `git commit` between the two looks, after any switch, is a
    /// commit made here; a reflog that cannot place both looks says no.
    #[test]
    fn a_commit_made_here_is_told_from_a_move_by_its_reflog() {
        let log = |entries: &[(&str, &str)]| {
            entries
                .iter()
                .map(|(commit, subject)| format!("{commit}\t{subject}\n"))
                .collect::<String>()
        };
        let made = log(&[
            ("c", "commit: second"),
            ("b", "commit (amend): first"),
            ("a", "checkout: moving from main to feature"),
        ]);
        assert!(committed_between(&made, "a", "c"));
        assert!(committed_between(&made, "b", "c"));
        let pulled = log(&[
            ("c", "pull: Fast-forward"),
            ("b", "merge upstream: Fast-forward"),
            ("a", "commit: mine, before the last look"),
        ]);
        assert!(!committed_between(&pulled, "a", "c"));
        // Moved since the head was read: counted from the entry for it.
        let later = log(&[
            ("d", "commit: after the read"),
            ("c", "reset: moving to c"),
            ("a", "commit: x"),
        ]);
        assert!(!committed_between(&later, "a", "c"));
        // A commit, then a switch back onto this branch: the commit was
        // on the other branch.
        let switched = log(&[
            ("c", "checkout: moving from other to feature"),
            ("b", "commit: on other"),
            ("a", "checkout: moving from feature to other"),
        ]);
        assert!(!committed_between(&switched, "a", "c"));
        // Undone by a reset back to where it was, then pulled.
        let undone = log(&[
            ("d", "pull: Fast-forward"),
            ("a", "reset: moving to HEAD~1"),
            ("b", "commit: scratch"),
            ("a", "commit: x"),
        ]);
        assert!(!committed_between(&undone, "a", "d"));
        // Neither head in the reflog, or no reflog: nothing is known.
        assert!(!committed_between(&made, "a", "z"));
        assert!(!committed_between(&log(&[("c", "commit: x")]), "a", "c"));
        assert!(!committed_between("", "a", "c"));
    }

    #[test]
    fn git_s_own_reflog_tells_a_commit_from_a_fast_forward_or_a_switch() {
        if !have_git() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let main = repo(&tmp);
        let head = |dir: &Path| git(tmp.path(), dir, &["rev-parse", "HEAD"]);
        let wait = std::time::Duration::from_secs(10);
        git(tmp.path(), &main, &["branch", "feature"]);
        let root = head(&main);
        // A commit made here.
        git(
            tmp.path(),
            &main,
            &["commit", "-q", "--allow-empty", "-m", "one"],
        );
        let one = head(&main);
        assert!(committed_here(&main, &root, &one, wait));
        // A fast-forward of one commit has the old head as its parent too.
        git(tmp.path(), &main, &["checkout", "-q", "feature"]);
        git(tmp.path(), &main, &["merge", "-q", "--ff-only", "main"]);
        assert_eq!(head(&main), one);
        assert!(!committed_here(&main, &root, &one, wait));
        // A switch is not a commit, and neither is a reset.
        git(tmp.path(), &main, &["checkout", "-q", "-b", "other"]);
        git(
            tmp.path(),
            &main,
            &["commit", "-q", "--allow-empty", "-m", "two"],
        );
        let two = head(&main);
        git(tmp.path(), &main, &["checkout", "-q", "feature"]);
        git(tmp.path(), &main, &["reset", "-q", "--hard", &two]);
        assert!(!committed_here(&main, &one, &two, wait));
        // Bad ids are not passed to git.
        assert!(!committed_here(&main, "HEAD", &two, wait));
        assert!(!committed_here(&main, &two, &two, wait));
    }
}
