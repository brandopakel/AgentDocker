//! The project watcher: file changes in every checkout a live agent works
//! in, turned into ledger entries and branch refreshes.
//!
//! One `notify` watcher (FSEvents on macOS, inotify on Linux) watches each
//! distinct checkout — the main root or a linked worktree — of every live
//! agent whose project is a repository or an `Agentfile.toml` root; plain
//! directories are not watched, because a recursive watch on a home
//! directory is exactly what inotify cannot afford. Raw events are
//! debounced for 100 ms, filtered through the checkout's `.gitignore` so
//! `target/` and `node_modules/` never reach the ledger, and `.git/` is
//! ignored except the files that say where HEAD is, which trigger a
//! branch refresh instead of an entry. Watches are reconciled against the
//! registry once a second, so an agent joining or leaving needs no hook.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agentdocker_core::ChangeKind;
use agentdocker_host::vcs;
use ignore::gitignore::Gitignore;
use notify::event::{EventKind, ModifyKind};
use notify::{RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::daemon::{Checkout, Daemon, Observed};

const RECONCILE_EVERY: Duration = Duration::from_secs(1);
const FLUSH_EVERY: Duration = Duration::from_millis(100);

struct Watched {
    checkout: Checkout,
    ignore: Gitignore,
    /// A linked worktree's own git directory, watched for its `HEAD`.
    gitdir: Option<PathBuf>,
}

pub fn spawn(daemon: Arc<Daemon>) {
    tokio::spawn(run(daemon, RECONCILE_EVERY, FLUSH_EVERY));
}

/// The watcher loop; intervals are parameters so tests can run it fast.
pub async fn run(daemon: Arc<Daemon>, reconcile_every: Duration, flush_every: Duration) {
    let (tx, mut rx) = mpsc::unbounded_channel::<notify::Event>();
    let mut watcher = match notify::recommended_watcher(
        move |result: notify::Result<notify::Event>| match result {
            Ok(event) => {
                let _ = tx.send(event);
            }
            Err(err) => warn!(%err, "watcher error"),
        },
    ) {
        Ok(watcher) => watcher,
        Err(err) => {
            error!(%err, "cannot start the project watcher; the ledger and branch tracking are off");
            return;
        }
    };
    let mut watched: BTreeMap<PathBuf, Watched> = BTreeMap::new();
    let mut pending: Vec<notify::Event> = Vec::new();
    let mut reconcile = tokio::time::interval(reconcile_every);
    let mut flush = tokio::time::interval(flush_every);
    loop {
        tokio::select! {
            _ = reconcile.tick() => reconcile_watches(&daemon, &mut watcher, &mut watched),
            Some(event) = rx.recv() => pending.push(event),
            _ = flush.tick() => {
                if pending.is_empty() {
                    continue;
                }
                let batch = std::mem::take(&mut pending);
                let (observed, vcs_touched) = classify(&batch, &watched);
                if !observed.is_empty() || !vcs_touched.is_empty() {
                    daemon.record_fs_changes(observed, vcs_touched).await;
                }
            }
        }
    }
}

fn reconcile_watches(
    daemon: &Daemon,
    watcher: &mut notify::RecommendedWatcher,
    watched: &mut BTreeMap<PathBuf, Watched>,
) {
    let wanted = daemon.watch_targets();
    let wanted_dirs: HashSet<&Path> = wanted.iter().map(|c| c.dir.as_path()).collect();
    let stale: Vec<PathBuf> = watched
        .keys()
        .filter(|dir| !wanted_dirs.contains(dir.as_path()))
        .cloned()
        .collect();
    for dir in stale {
        if let Some(entry) = watched.remove(&dir) {
            let _ = watcher.unwatch(&dir);
            if let Some(gitdir) = &entry.gitdir {
                let _ = watcher.unwatch(gitdir);
            }
            info!(checkout = %dir.display(), "stopped watching");
        }
    }
    for checkout in wanted {
        if watched.contains_key(&checkout.dir) {
            continue;
        }
        if let Err(err) = watcher.watch(&checkout.dir, RecursiveMode::Recursive) {
            warn!(checkout = %checkout.dir.display(), %err, "cannot watch checkout");
            continue;
        }
        // A linked worktree keeps HEAD in its own git directory, elsewhere.
        let own_git = checkout.dir.join(".git");
        let gitdir = vcs::git_dirs(&checkout.dir)
            .map(|(gitdir, _)| gitdir)
            .filter(|gitdir| *gitdir != own_git)
            .filter(|gitdir| watcher.watch(gitdir, RecursiveMode::NonRecursive).is_ok());
        let (ignore, problem) = Gitignore::new(checkout.dir.join(".gitignore"));
        if let Some(problem) = problem {
            debug!(checkout = %checkout.dir.display(), %problem, "no usable .gitignore");
        }
        info!(checkout = %checkout.dir.display(), project = %checkout.project.short(), "watching");
        watched.insert(
            checkout.dir.clone(),
            Watched {
                checkout,
                ignore,
                gitdir,
            },
        );
    }
}

/// Turn a debounced batch into ledger observations plus the checkouts
/// whose HEAD moved. Duplicates within the batch collapse to one.
fn classify(
    batch: &[notify::Event],
    watched: &BTreeMap<PathBuf, Watched>,
) -> (Vec<Observed>, Vec<Checkout>) {
    let mut observed: Vec<Observed> = Vec::new();
    let mut seen: HashSet<(PathBuf, PathBuf, ChangeKind)> = HashSet::new();
    let mut vcs_touched: Vec<Checkout> = Vec::new();
    let mut vcs_dirs: HashSet<PathBuf> = HashSet::new();

    for event in batch {
        let Some(kind) = kind_of(&event.kind) else {
            continue;
        };
        for path in &event.paths {
            let Some(entry) = owner(path, watched) else {
                continue;
            };
            let dir = &entry.checkout.dir;
            if entry.gitdir.as_ref().is_some_and(|g| path.starts_with(g)) {
                if head_moved(
                    path.strip_prefix(entry.gitdir.as_ref().unwrap())
                        .unwrap_or(path),
                ) && vcs_dirs.insert(dir.clone())
                {
                    vcs_touched.push(entry.checkout.clone());
                }
                continue;
            }
            let Ok(relative) = path.strip_prefix(dir) else {
                continue;
            };
            if let Ok(inside_git) = relative.strip_prefix(".git") {
                if head_moved(inside_git) && vcs_dirs.insert(dir.clone()) {
                    vcs_touched.push(entry.checkout.clone());
                }
                continue;
            }
            let is_dir = path.is_dir();
            if is_dir && kind != ChangeKind::Removed {
                continue;
            }
            if entry
                .ignore
                .matched_path_or_any_parents(path, is_dir)
                .is_ignore()
            {
                continue;
            }
            if seen.insert((dir.clone(), relative.to_path_buf(), kind)) {
                observed.push(Observed {
                    checkout: entry.checkout.clone(),
                    path: relative.to_path_buf(),
                    kind,
                });
            }
        }
    }
    (observed, vcs_touched)
}

/// The watched checkout containing `path`: the deepest one, so a worktree
/// nested under a root wins over the root.
fn owner<'a>(path: &Path, watched: &'a BTreeMap<PathBuf, Watched>) -> Option<&'a Watched> {
    watched
        .values()
        .filter(|w| {
            path.starts_with(&w.checkout.dir)
                || w.gitdir.as_ref().is_some_and(|g| path.starts_with(g))
        })
        .max_by_key(|w| w.checkout.dir.as_os_str().len())
}

/// Files inside a git directory that say where HEAD is.
fn head_moved(inside_git: &Path) -> bool {
    let text = inside_git.to_string_lossy();
    text == "HEAD"
        || text == "packed-refs"
        || text.starts_with("refs/heads")
        || (text.starts_with("worktrees/") && text.ends_with("/HEAD"))
}

fn kind_of(kind: &EventKind) -> Option<ChangeKind> {
    match kind {
        EventKind::Create(_) => Some(ChangeKind::Created),
        EventKind::Modify(ModifyKind::Name(_)) => Some(ChangeKind::Renamed),
        EventKind::Modify(_) | EventKind::Any | EventKind::Other => Some(ChangeKind::Modified),
        EventKind::Remove(_) => Some(ChangeKind::Removed),
        EventKind::Access(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_files_are_recognised() {
        assert!(head_moved(Path::new("HEAD")));
        assert!(head_moved(Path::new("packed-refs")));
        assert!(head_moved(Path::new("refs/heads/main")));
        assert!(head_moved(Path::new("refs/heads/feature/x")));
        assert!(head_moved(Path::new("worktrees/wt/HEAD")));
        assert!(!head_moved(Path::new("objects/ab/cdef")));
        assert!(!head_moved(Path::new("index")));
        assert!(!head_moved(Path::new("refs/remotes/origin/main")));
    }

    #[test]
    fn notify_kinds_map_to_ledger_kinds() {
        use notify::event::{CreateKind, DataChange, RemoveKind, RenameMode};
        assert_eq!(
            kind_of(&EventKind::Create(CreateKind::File)),
            Some(ChangeKind::Created)
        );
        assert_eq!(
            kind_of(&EventKind::Modify(ModifyKind::Data(DataChange::Content))),
            Some(ChangeKind::Modified)
        );
        assert_eq!(
            kind_of(&EventKind::Modify(ModifyKind::Name(RenameMode::To))),
            Some(ChangeKind::Renamed)
        );
        assert_eq!(
            kind_of(&EventKind::Remove(RemoveKind::File)),
            Some(ChangeKind::Removed)
        );
        assert_eq!(
            kind_of(&EventKind::Access(notify::event::AccessKind::Read)),
            None
        );
    }
}
