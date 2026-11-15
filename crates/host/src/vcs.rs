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
    worktree_list(dir, std::time::Instant::now() + timeout).unwrap_or_default()
}

/// What kept a git read from answering.
#[derive(Debug)]
pub enum ReadError {
    /// Git could not be started.
    Spawn(std::io::Error),
    /// It did not finish by its deadline, and was stopped.
    TimedOut,
    /// It finished and said no: the first line it wrote to stderr.
    Failed(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "git could not be started: {error}"),
            Self::TimedOut => f.write_str("git did not answer in time"),
            Self::Failed(said) if said.is_empty() => f.write_str("git failed"),
            Self::Failed(said) => write!(f, "git: {said}"),
        }
    }
}

impl std::error::Error for ReadError {}

/// `git <args>`'s standard output, run in `dir` by
/// [`crate::command::run_with_env`]: its own process group, stopped whole
/// at `deadline` with whatever helpers git started (an fsmonitor hook,
/// say), and its output written to files rather than pipes, so a helper
/// that outlives git and keeps a descriptor never holds the read past git's
/// exit. At most 4 MiB of output.
fn git_output(
    dir: &Path,
    args: &[&str],
    deadline: std::time::Instant,
) -> Result<String, ReadError> {
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    if left.is_zero() {
        return Err(ReadError::TimedOut);
    }
    if !dir.is_dir() {
        return Err(ReadError::Failed(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    let argv: Vec<String> = std::iter::once("git")
        .chain(args.iter().copied())
        .map(str::to_owned)
        .collect();
    let environment = [
        ("GIT_OPTIONAL_LOCKS", Some(std::ffi::OsStr::new("0"))),
        ("LC_ALL", Some(std::ffi::OsStr::new("C"))),
    ];
    let output = match crate::command::run_with_env(dir, &argv, left, &environment) {
        Ok(output) => output,
        // It stops a command only at its timeout, which is our deadline.
        Err(_) if std::time::Instant::now() >= deadline => return Err(ReadError::TimedOut),
        Err(error) if error.kind() == std::io::ErrorKind::Other => {
            return Err(ReadError::Failed(error.to_string()));
        }
        Err(error) => return Err(ReadError::Spawn(error)),
    };
    if !output.success {
        // The text is standard output, then standard error.
        let said = output.text.get(output.stdout.len()..).unwrap_or_default();
        return Err(ReadError::Failed(
            said.lines().next().unwrap_or_default().trim().to_owned(),
        ));
    }
    Ok(output.stdout)
}

/// The checkouts of the repository `dir` is in, as `git worktree list`
/// gives them (the main one first), asked within `deadline`.
pub fn worktree_list(dir: &Path, deadline: std::time::Instant) -> Result<Vec<PathBuf>, ReadError> {
    let out = git_output(dir, &["worktree", "list", "--porcelain"], deadline)?;
    Ok(out
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(|path| {
            let path = PathBuf::from(path);
            path.canonicalize().unwrap_or(path)
        })
        .collect())
}

/// The branch the repository's main checkout is on, as `git worktree list`
/// gives it: none when it is detached or bare.
fn main_checkout_branch(
    dir: &Path,
    deadline: std::time::Instant,
) -> Result<Option<String>, ReadError> {
    let out = git_output(dir, &["worktree", "list", "--porcelain"], deadline)?;
    Ok(out
        .split("\n\n")
        .next()
        .and_then(|main| main.lines().find_map(|line| line.strip_prefix("branch ")))
        .map(|full| full.strip_prefix("refs/heads/").unwrap_or(full).to_owned()))
}

/// The paths of the checkout `dir` that differ from its HEAD: modified,
/// added and deleted ones, both sides of a rename, and untracked files
/// that are not ignored, relative to the checkout's root as git prints
/// them, in git's order. Asked of git within `deadline`.
pub fn dirty_paths(dir: &Path, deadline: std::time::Instant) -> Result<Vec<String>, ReadError> {
    let out = git_output(
        dir,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ],
        deadline,
    )?;
    // Each entry is two status letters, a space and the path.
    Ok(out
        .split('\0')
        .filter_map(|entry| entry.get(3..))
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Where the checkout `dir` stands, for a brief: its branch and head
/// (read from `.git`), its uncommitted paths ([`dirty_paths`]), what
/// `origin/<branch>` names, the commits not pushed (counted against the
/// branch's upstream, else `origin/<branch>`), and the commits its base
/// lacks, the base being what `origin/HEAD` names, else `origin/main`,
/// else `origin/master`, else the branch the repository's main checkout is
/// on, which is marked local (`base_is_local`): being in it is no sign of
/// having been pushed. Local refs only, never a fetch. Every git call
/// shares `deadline`.
/// A ref that is not there is an answer (no upstream, never pushed, no
/// base); a count git could not give is left `None`, which
/// [`agentdocker_core::brief::Standing::stands`] never reads as no work.
pub fn standing(
    dir: &Path,
    deadline: std::time::Instant,
) -> Result<agentdocker_core::brief::Standing, ReadError> {
    let (branch, head) = state(dir).map_or((None, None), |s| (s.branch, s.head));
    let dirty = dirty_paths(dir, deadline)?;
    let mut standing = agentdocker_core::brief::Standing {
        branch,
        head,
        dirty,
        ..Default::default()
    };
    if standing.head.is_none() {
        // No commit yet: nothing to push or merge.
        return Ok(standing);
    }
    let ask = |args: &[&str]| match git_output(dir, args, deadline) {
        Ok(out) => Ok(Some(out.trim().to_owned()).filter(|s| !s.is_empty())),
        Err(ReadError::Failed(_)) => Ok(None),
        Err(error) => Err(error),
    };
    // A ref, by its full name, when it names a commit.
    let exists = |full: &str| -> Result<bool, ReadError> {
        Ok(ask(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{full}^{{commit}}"),
        ])?
        .is_some())
    };
    let count = |full: &str| -> Result<Option<usize>, ReadError> {
        Ok(ask(&["rev-list", "--count", &format!("{full}..HEAD")])?.and_then(|n| n.parse().ok()))
    };
    if let Some(branch) = standing.branch.clone() {
        let remote = format!("refs/remotes/origin/{branch}");
        standing.pushed = ask(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{remote}^{{commit}}"),
        ])?;
        // An upstream that is a branch of this repository (`branch.<b>.remote
        // = .`) is no remote: what is on it was never pushed.
        let upstream = ask(&["rev-parse", "--symbolic-full-name", "@{upstream}"])?
            .filter(|full| full.starts_with("refs/remotes/"));
        // What the commits not pushed are counted against: the upstream,
        // else the branch's own name on origin.
        let against = match upstream {
            Some(full) => Some(full),
            None => standing.pushed.is_some().then_some(remote),
        };
        if let Some(full) = against {
            standing.unpushed = count(&full)?;
            standing.upstream = Some(short_ref(&full));
        }
    }
    let base = match ask(&["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"])? {
        Some(full) => Some(full),
        None if exists("refs/remotes/origin/main")? => Some("refs/remotes/origin/main".to_owned()),
        None if exists("refs/remotes/origin/master")? => {
            Some("refs/remotes/origin/master".to_owned())
        }
        None => match main_checkout_branch(dir, deadline) {
            Ok(Some(branch)) => Some(format!("refs/heads/{branch}")),
            Ok(None) | Err(ReadError::Failed(_)) => None,
            Err(error) => return Err(error),
        },
    };
    if let Some(full) = base {
        standing.unmerged = count(&full)?;
        standing.base_is_local = full.starts_with("refs/heads/");
        standing.base = Some(short_ref(&full));
    }
    Ok(standing)
}

/// A ref as a person names it: `origin/main` for `refs/remotes/origin/main`,
/// `main` for `refs/heads/main`.
fn short_ref(full: &str) -> String {
    full.strip_prefix("refs/remotes/")
        .or_else(|| full.strip_prefix("refs/heads/"))
        .unwrap_or(full)
        .to_owned()
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

    /// A checkout's standing comes from local refs: its uncommitted paths
    /// (untracked ones too), the commits its upstream lacks, what
    /// `origin/<branch>` names, and the commits its base lacks; a branch
    /// never pushed has neither an upstream nor a pushed ref; and a read
    /// past its deadline is stopped.
    #[test]
    fn a_checkouts_standing_is_read_from_local_refs() {
        if !have_git() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let remote = tmp.path().join("remote.git");
        let work = tmp.path().join("work");
        git(
            home,
            home,
            &["init", "-q", "--bare", remote.to_str().unwrap()],
        );
        git(
            home,
            home,
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                work.to_str().unwrap(),
            ],
        );
        let commit = |name: &str| {
            fs::write(work.join(format!("{name}.txt")), name).unwrap();
            git(home, &work, &["add", "-A"]);
            git(home, &work, &["commit", "-q", "-m", name]);
        };
        commit("one");
        git(home, &work, &["push", "-q", "origin", "main"]);
        git(home, &work, &["remote", "set-head", "origin", "main"]);
        git(home, &work, &["checkout", "-q", "-b", "feat"]);
        commit("two");
        git(home, &work, &["push", "-q", "-u", "origin", "feat"]);
        let pushed = git(home, &work, &["rev-parse", "HEAD"]);
        commit("three");
        fs::write(work.join("one.txt"), "changed").unwrap();
        fs::write(work.join("new.txt"), "new").unwrap();

        let deadline = || std::time::Instant::now() + std::time::Duration::from_secs(20);
        let read = standing(&work, deadline()).unwrap();
        assert_eq!(read.branch.as_deref(), Some("feat"));
        assert_eq!(read.dirty, ["one.txt", "new.txt"]);
        assert_eq!(read.upstream.as_deref(), Some("origin/feat"));
        assert_eq!(read.unpushed, Some(1));
        assert_eq!(read.pushed.as_deref(), Some(pushed.as_str()));
        assert_eq!(read.base.as_deref(), Some("origin/main"));
        assert_eq!(read.unmerged, Some(2));
        assert!(read.stands());

        git(home, &work, &["checkout", "-q", "-b", "local"]);
        commit("four");
        let read = standing(&work, deadline()).unwrap();
        assert_eq!(read.branch.as_deref(), Some("local"));
        assert!(read.dirty.is_empty(), "{:?}", read.dirty);
        assert_eq!(
            (read.upstream, read.unpushed, read.pushed),
            (None, None, None)
        );
        assert_eq!(read.unmerged, Some(3), "two, three and four");

        // Pushed with no upstream set: counted against its name on origin.
        git(home, &work, &["checkout", "-q", "-b", "plain"]);
        git(home, &work, &["push", "-q", "origin", "plain"]);
        commit("five");
        let read = standing(&work, deadline()).unwrap();
        assert_eq!(read.upstream.as_deref(), Some("origin/plain"));
        assert_eq!(read.unpushed, Some(1));
        assert!(read.pushed.is_some());

        assert!(matches!(
            dirty_paths(&work, std::time::Instant::now()),
            Err(ReadError::TimedOut)
        ));
        assert!(matches!(
            dirty_paths(&tmp.path().join("nowhere"), deadline()),
            Err(ReadError::Failed(_))
        ));
    }

    /// With no remote, the base is the branch the main checkout is on,
    /// marked local: the main checkout is merged into it but never pushed,
    /// so its work still stands, and a linked worktree's commits are
    /// counted against it, never pushed and not merged.
    #[test]
    fn a_repository_without_a_remote_is_measured_against_its_main_checkout() {
        if !have_git() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let main = repo(&tmp);
        let deadline = || std::time::Instant::now() + std::time::Duration::from_secs(20);
        let read = standing(&main, deadline()).unwrap();
        assert_eq!(read.base.as_deref(), Some("main"));
        assert_eq!(read.unmerged, Some(0));
        assert!(read.base_is_local);
        assert!(!read.on_remote());
        assert!(read.stands(), "merged into itself, but never pushed");
        assert_eq!((read.upstream, read.pushed), (None, None));

        let linked = tmp.path().join("linked");
        git(
            tmp.path(),
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "topic",
                linked.to_str().unwrap(),
            ],
        );
        git(
            tmp.path(),
            &linked,
            &["commit", "-q", "--allow-empty", "-m", "topic"],
        );
        let read = standing(&linked, deadline()).unwrap();
        assert_eq!(read.branch.as_deref(), Some("topic"));
        assert_eq!(read.base.as_deref(), Some("main"));
        assert_eq!(read.unmerged, Some(1));
        assert!(read.stands(), "committed, never pushed, not merged");
        assert_eq!(
            (read.upstream, read.unpushed, read.pushed),
            (None, None, None)
        );
        assert_eq!(
            worktree_list(&linked, deadline()).unwrap(),
            [main.canonicalize().unwrap(), linked.canonicalize().unwrap()]
        );
    }

    /// A helper git starts (an fsmonitor hook, here) is stopped with git at
    /// the deadline; one that outlives git, holding the standard error git
    /// gave it, does not hold the read past git's exit.
    #[cfg(unix)]
    #[test]
    fn a_read_is_bounded_whatever_gits_helpers_do() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{Duration, Instant};
        if !have_git() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let main = repo(&tmp);
        let hook = tmp.path().join("fsmonitor.sh");
        let pid_file = tmp.path().join("helper.pid");
        let write_hook = |body: &str| {
            fs::write(&hook, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        };
        git(
            tmp.path(),
            &main,
            &["config", "core.fsmonitor", hook.to_str().unwrap()],
        );
        let helper = || -> i32 {
            fs::read_to_string(&pid_file)
                .expect("git ran the hook")
                .trim()
                .parse()
                .unwrap()
        };
        // SAFETY: signal 0 only asks whether the process is there.
        let alive = |pid: i32| unsafe { libc::kill(pid, 0) } == 0;

        // The hook waits on its helper, so git waits on the hook.
        write_hook(&format!(
            "sleep 30 &\necho $! > '{}'\nwait",
            pid_file.display()
        ));
        let started = Instant::now();
        let read = dirty_paths(&main, started + Duration::from_millis(500));
        assert!(matches!(read, Err(ReadError::TimedOut)), "{read:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
        let pid = helper();
        let gone = (0..300).any(|_| {
            std::thread::sleep(Duration::from_millis(10));
            !alive(pid)
        });
        assert!(gone, "the helper is stopped with git");

        // The helper outlives the hook and git, keeping standard error.
        fs::remove_file(&pid_file).unwrap();
        write_hook(&format!(
            "sleep 30 >/dev/null &\necho $! > '{}'\nexit 1",
            pid_file.display()
        ));
        let started = Instant::now();
        let read = dirty_paths(&main, started + Duration::from_secs(20));
        let took = started.elapsed();
        // SAFETY: the helper this test started, by the pid it wrote.
        unsafe { libc::kill(helper(), libc::SIGKILL) };
        assert!(read.is_ok(), "{read:?}");
        assert!(took < Duration::from_secs(10), "held for {took:?}");
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
}
