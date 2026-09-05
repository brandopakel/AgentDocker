//! Which project a working directory belongs to.
//!
//! Discovery walks up from the working directory. A `.git` entry marks a
//! repository; a `.git` *file* is a linked worktree or a submodule, and a
//! worktree resolves to its main repository so every worktree of one repo
//! is the same project. Failing that, an `Agentfile.toml` marks a root, and
//! failing that the directory is its own project.
//!
//! Fingerprinting shells out to `git` and is a separate step because it
//! walks the whole history, which can take seconds on huge repositories;
//! the daemon caches it per root.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use agentdocker_core::{ProjectRef, ProjectSource};

/// The file that marks a project root when there is no repository.
pub const AGENTFILE: &str = "Agentfile.toml";

/// The project containing `workdir`. Never fails: a directory with no
/// markers above it is its own project. The result carries no fingerprint;
/// see [`fingerprint`].
pub fn discover(workdir: &Path) -> ProjectRef {
    let start = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());
    if let Some(project) = git_project(&start) {
        return project;
    }
    if let Some(root) = start.ancestors().find(|dir| dir.join(AGENTFILE).is_file()) {
        return ProjectRef {
            root: root.to_path_buf(),
            worktree: None,
            fingerprint: None,
            source: ProjectSource::Agentfile,
        };
    }
    ProjectRef::directory(start)
}

/// Canonicalise as much of `path` as exists: the longest existing ancestor
/// is resolved and the rest appended unchanged, so a file about to be
/// created gets the key it will have once it exists (`/tmp` on macOS is
/// `/private/tmp` either way).
pub fn canonical(path: &Path) -> PathBuf {
    for ancestor in path.ancestors() {
        if let Ok(base) = ancestor.canonicalize() {
            return match path.strip_prefix(ancestor) {
                Ok(rest) if !rest.as_os_str().is_empty() => base.join(rest),
                _ => base,
            };
        }
    }
    path.to_path_buf()
}

fn git_project(start: &Path) -> Option<ProjectRef> {
    for dir in start.ancestors() {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            return Some(repository(dir.to_path_buf(), None));
        }
        if dot_git.is_file() {
            return Some(linked(dir, &dot_git));
        }
    }
    None
}

fn repository(root: PathBuf, worktree: Option<PathBuf>) -> ProjectRef {
    ProjectRef {
        root,
        worktree,
        fingerprint: None,
        source: ProjectSource::Git,
    }
}

/// A `.git` file reads `gitdir: <path>`. A linked worktree's git directory
/// contains `commondir`, which points at the main repository's `.git`; a
/// submodule's does not, and a submodule is its own project.
fn linked(dir: &Path, dot_git: &Path) -> ProjectRef {
    let main_root = fs::read_to_string(dot_git)
        .ok()
        .and_then(|text| {
            text.trim()
                .strip_prefix("gitdir:")
                .map(|path| resolve(dir, path.trim()))
        })
        .and_then(|gitdir| {
            let common = fs::read_to_string(gitdir.join("commondir")).ok()?;
            let common = resolve(&gitdir, common.trim());
            let common = common.canonicalize().unwrap_or(common);
            common.parent().map(Path::to_path_buf)
        });
    match main_root {
        Some(root) if root != dir => repository(root, Some(dir.to_path_buf())),
        _ => repository(dir.to_path_buf(), None),
    }
}

fn resolve(base: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// The repository's oldest root commit — the smallest hash when unrelated
/// histories were merged, so the answer is stable. `None` when `git` is
/// missing, the repository has no commits, or `timeout` passes first.
pub fn fingerprint(root: &Path, timeout: Duration) -> Option<String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-list", "--max-parents=0", "HEAD"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
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
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    out.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .min()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const TIMEOUT: Duration = Duration::from_secs(10);

    /// Run git with no user or system configuration in the way.
    fn git(home: &Path, dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
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
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn have_git() -> bool {
        Command::new("git")
            .arg("--version")
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// A repository with one empty root commit under `<tmp>/main`.
    fn repo(tmp: &TempDir) -> PathBuf {
        let main = tmp.path().join("main");
        fs::create_dir_all(&main).unwrap();
        assert!(git(tmp.path(), &main, &["init", "-q"]));
        assert!(git(
            tmp.path(),
            &main,
            &["commit", "-q", "--allow-empty", "-m", "root"]
        ));
        main.canonicalize().unwrap()
    }

    #[test]
    fn directory_without_markers_is_its_own_project() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("plain/nested");
        fs::create_dir_all(&dir).unwrap();
        let project = discover(&dir);
        assert_eq!(project.source, ProjectSource::Directory);
        assert_eq!(project.root, dir.canonicalize().unwrap());
        assert_eq!(project.worktree, None);
    }

    #[test]
    fn canonical_resolves_the_existing_prefix_only() {
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().canonicalize().unwrap();
        assert_eq!(canonical(tmp.path()), real);
        assert_eq!(
            canonical(&tmp.path().join("new/deeper/file.rs")),
            real.join("new/deeper/file.rs")
        );
        let nowhere = Path::new("/definitely/not/here/x");
        assert_eq!(canonical(nowhere), Path::new("/definitely/not/here/x"));
    }

    #[test]
    fn missing_directory_is_still_a_project() {
        let project = discover(Path::new("/definitely/not/here"));
        assert_eq!(project.source, ProjectSource::Directory);
        assert_eq!(project.root, Path::new("/definitely/not/here"));
    }

    #[test]
    fn agentfile_marks_a_root() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("team");
        let nested = root.join("services/api");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join(AGENTFILE), "").unwrap();
        let project = discover(&nested);
        assert_eq!(project.source, ProjectSource::Agentfile);
        assert_eq!(project.root, root.canonicalize().unwrap());
    }

    #[test]
    fn git_root_and_nested_directory_share_a_project() {
        if !have_git() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let main = repo(&tmp);
        let nested = main.join("src/deep");
        fs::create_dir_all(&nested).unwrap();

        let at_root = discover(&main);
        let below = discover(&nested);
        assert_eq!(at_root.source, ProjectSource::Git);
        assert_eq!(at_root.root, main);
        assert_eq!(at_root.worktree, None);
        assert_eq!(below, at_root);
        assert_eq!(below.id(), at_root.id());
    }

    #[test]
    fn linked_worktree_resolves_to_the_main_repository() {
        if !have_git() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let main = repo(&tmp);
        let wt = tmp.path().join("wt");
        assert!(git(
            tmp.path(),
            &main,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "wt"]
        ));
        let wt = wt.canonicalize().unwrap();
        let inside = wt.join("src");
        fs::create_dir_all(&inside).unwrap();

        let project = discover(&inside);
        assert_eq!(project.source, ProjectSource::Git);
        assert_eq!(project.root, main);
        assert_eq!(project.worktree, Some(wt.clone()));
        assert_eq!(project.dir(), wt.as_path());
        assert_eq!(project.id(), discover(&main).id());
    }

    #[test]
    fn fingerprint_is_the_root_commit_everywhere_in_the_repository() {
        if !have_git() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let main = repo(&tmp);
        assert!(git(
            tmp.path(),
            &main,
            &["commit", "-q", "--allow-empty", "-m", "second"]
        ));
        let wt = tmp.path().join("wt");
        assert!(git(
            tmp.path(),
            &main,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "wt"]
        ));

        let root = fingerprint(&main, TIMEOUT).expect("a root commit");
        assert_eq!(root.len(), 40);
        assert_eq!(fingerprint(&wt, TIMEOUT).as_deref(), Some(root.as_str()));
        assert_eq!(fingerprint(&main.join("src"), TIMEOUT), None, "missing dir");
    }

    #[test]
    fn fingerprint_is_none_without_commits_or_repository() {
        if !have_git() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let empty = tmp.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        assert!(git(tmp.path(), &empty, &["init", "-q"]));
        assert_eq!(fingerprint(&empty, TIMEOUT), None);
        assert_eq!(fingerprint(tmp.path(), TIMEOUT), None);
    }
}
