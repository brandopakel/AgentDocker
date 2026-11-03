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

/// The file a project keeps its worktree includes in: one git pathspec per
/// line of files git ignores that a worktree still needs (`.env`, local
/// settings). The name is the one other session managers read.
pub const WORKTREE_INCLUDE: &str = ".worktreeinclude";

/// The `git` arguments that list, NUL-separated, the files a
/// `.worktreeinclude` reading `text` names: those its pathspecs match that
/// git ignores, and so that no worktree made from a commit has. Blank
/// lines, `#` comments and lines starting with `-` (which git would read
/// as an option) name nothing; `None` when nothing is named. The daemon
/// copies what this lists into a worktree it makes, and a client asking
/// what such a worktree will hold lists it the same way.
pub fn worktree_include_listing(text: &str) -> Option<Vec<String>> {
    let specs: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('-'))
        .map(str::to_owned)
        .collect();
    if specs.is_empty() {
        return None;
    }
    let mut args: Vec<String> = [
        "ls-files",
        "-z",
        "--others",
        "--ignored",
        "--exclude-standard",
        "--",
    ]
    .map(str::to_owned)
    .into();
    args.extend(specs);
    Some(args)
}

/// At most this many bytes of what a `.worktreeinclude` names are copied
/// into one worktree.
pub const WORKTREE_INCLUDE_BYTES: u64 = 256 * 1024 * 1024;

/// What a copy of a `.worktreeinclude` listing takes from the checkout
/// `root`: of the paths `listed` names, NUL-separated as the command
/// [`worktree_include_listing`] gives prints them, in order, the regular
/// files — a symlink is left out, since it could point anywhere, and so is
/// anything else that is not a file — each with its metadata, until
/// together they pass [`WORKTREE_INCLUDE_BYTES`], where the copy stops.
/// The second value says why it stopped early, when it did. The daemon
/// copies by this, and a client predicting a new worktree's contents reads
/// it the same way.
pub fn worktree_include_sources(
    root: &Path,
    listed: &str,
) -> (Vec<(String, std::fs::Metadata)>, Option<String>) {
    include_sources_within(root, listed, WORKTREE_INCLUDE_BYTES)
}

fn include_sources_within(
    root: &Path,
    listed: &str,
    budget: u64,
) -> (Vec<(String, std::fs::Metadata)>, Option<String>) {
    let (mut sources, mut bytes) = (Vec::new(), 0u64);
    for relative in listed.split('\0').filter(|r| !r.is_empty()) {
        let Ok(meta) = std::fs::symlink_metadata(root.join(relative)) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        bytes += meta.len();
        if bytes > budget {
            let limit = budget / (1024 * 1024);
            return (
                sources,
                Some(format!(
                    "{WORKTREE_INCLUDE} names more than {limit} MiB; the rest was not copied"
                )),
            );
        }
        sources.push((relative.to_owned(), meta));
    }
    (sources, None)
}

/// Whether a copy may write `relative` beneath a new worktree: an ordinary
/// relative path none of whose prefixes, the file itself included, is a
/// symlink there, as `is_symlink` says of each prefix in turn (relative to
/// the worktree). Written through a symlink, the copy would land wherever
/// it points, and a checkout made from somebody's commit can carry one.
pub fn worktree_include_writable(
    relative: &str,
    mut is_symlink: impl FnMut(&Path) -> bool,
) -> bool {
    let mut at = PathBuf::new();
    for part in Path::new(relative).components() {
        let std::path::Component::Normal(name) = part else {
            return false;
        };
        at.push(name);
        if is_symlink(&at) {
            return false;
        }
    }
    true
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

    /// A copy takes regular files in the order listed, never a symlink or
    /// a folder, and stops once the files together pass the budget.
    #[cfg(unix)]
    #[test]
    fn a_worktree_include_copies_regular_files_within_its_budget() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("dir")).unwrap();
        fs::write(root.join("a.env"), "1234").unwrap();
        fs::write(root.join("b.env"), "56").unwrap();
        fs::write(root.join("outside"), "x").unwrap();
        std::os::unix::fs::symlink(root.join("outside"), root.join("link.env")).unwrap();
        let listed = "a.env\0dir\0link.env\0missing\0b.env\0";
        let names = |(sources, _): &(Vec<(String, std::fs::Metadata)>, Option<String>)| {
            sources
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
        };
        let all = worktree_include_sources(root, listed);
        assert_eq!(names(&all), ["a.env", "b.env"]);
        assert_eq!(all.1, None);
        let short = include_sources_within(root, listed, 5);
        assert_eq!(names(&short), ["a.env"], "b.env would pass the budget");
        assert!(short.1.unwrap().contains("the rest was not copied"));
    }

    #[test]
    fn a_worktree_include_is_never_written_through_a_symlink() {
        let symlinks = ["shared", "a/b"];
        let is_symlink = |at: &Path| symlinks.iter().any(|link| at == Path::new(link));
        assert!(worktree_include_writable(
            ".claude/settings.local.json",
            is_symlink
        ));
        assert!(worktree_include_writable("a/c", is_symlink));
        assert!(
            !worktree_include_writable("shared/x", is_symlink),
            "a folder on the way"
        );
        assert!(
            !worktree_include_writable("a/b", is_symlink),
            "the file itself"
        );
        assert!(!worktree_include_writable("../x", is_symlink));
        assert!(!worktree_include_writable("/etc/x", is_symlink));
        assert!(!worktree_include_writable("a/./b/../c", is_symlink));
    }

    #[test]
    fn a_worktree_include_names_pathspecs_and_nothing_else() {
        assert_eq!(worktree_include_listing(""), None);
        assert_eq!(
            worktree_include_listing("# only a comment\n\n  \n-x\n"),
            None
        );
        assert_eq!(
            worktree_include_listing(" .env \n# keys\n.claude/settings.local.json\n--all\n"),
            Some(
                [
                    "ls-files",
                    "-z",
                    "--others",
                    "--ignored",
                    "--exclude-standard",
                    "--",
                    ".env",
                    ".claude/settings.local.json",
                ]
                .map(str::to_owned)
                .into()
            )
        );
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
