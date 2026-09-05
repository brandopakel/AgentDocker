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
//! registry once a second, and at once when an agent registers — the
//! registration waits for it — so a checkout is covered before its first
//! agent's first edit, and an agent leaving needs no hook.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use agentdocker_core::ChangeKind;
use agentdocker_host::vcs;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::event::{EventKind, ModifyKind};
use notify::{RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::daemon::{Checkout, Daemon, Observed};

const RECONCILE_EVERY: Duration = Duration::from_secs(1);
const FLUSH_EVERY: Duration = Duration::from_millis(100);
/// How long an on-demand flush waits for the OS before draining.
const FLUSH_GRACE: Duration = Duration::from_millis(150);

struct Watched {
    checkout: Checkout,
    /// A linked worktree's own git directory, watched for its `HEAD`.
    gitdir: Option<PathBuf>,
}

pub fn spawn(daemon: Arc<Daemon>) {
    tokio::spawn(run(daemon, RECONCILE_EVERY, FLUSH_EVERY));
}

/// The watcher loop; intervals are parameters so tests can run it fast.
pub async fn run(daemon: Arc<Daemon>, reconcile_every: Duration, flush_every: Duration) {
    let (tx, mut rx) = mpsc::channel::<notify::Event>(4096);
    let gap = Arc::new(AtomicBool::new(false));
    let callback_gap = gap.clone();
    let mut watcher = match notify::recommended_watcher(
        move |result: notify::Result<notify::Event>| match result {
            Ok(event) => {
                if kind_of(&event.kind).is_some() && tx.try_send(event).is_err() {
                    callback_gap.store(true, Ordering::Relaxed);
                }
            }
            Err(err) => {
                callback_gap.store(true, Ordering::Relaxed);
                warn!(%err, "watcher error");
            }
        },
    ) {
        Ok(watcher) => watcher,
        Err(err) => {
            error!(%err, "cannot start the project watcher; the ledger and branch tracking are off");
            return;
        }
    };
    let mut watched: BTreeMap<PathBuf, Watched> = BTreeMap::new();
    let mut retries: HashMap<PathBuf, std::time::Instant> = HashMap::new();
    let mut pending: Vec<notify::Event> = Vec::new();
    let mut reconcile = tokio::time::interval(reconcile_every);
    let mut flush = tokio::time::interval(flush_every);
    // A release asks for an immediate flush through this channel, so its
    // journal entry sees the changes made just before it.
    let (flush_tx, mut flush_rx) =
        tokio::sync::mpsc::channel::<tokio::sync::oneshot::Sender<()>>(8);
    daemon.set_watcher_flush(flush_tx);
    // A registration asks for an immediate reconcile through this channel,
    // so the new agent's checkout is watched before the reply goes out.
    let (attach_tx, mut attach_rx) =
        tokio::sync::mpsc::channel::<tokio::sync::oneshot::Sender<()>>(8);
    daemon.set_watcher_attach(attach_tx);
    loop {
        tokio::select! {
            _ = reconcile.tick() => reconcile_watches(&daemon, &mut watcher, &mut watched, &mut retries),
            Some(ack) = attach_rx.recv() => {
                reconcile_watches(&daemon, &mut watcher, &mut watched, &mut retries);
                let _ = ack.send(());
            }
            Some(event) = rx.recv() => {
                if pending.len() < 4096 { pending.push(event); }
                else { gap.store(true, Ordering::Relaxed); }
            },
            Some(ack) = flush_rx.recv() => {
                // Give the OS a moment to deliver what just happened (FSEvents
                // batches with some latency), take what is queued, then flush.
                // No filesystem events can arrive when no checkout is watched.
                // Plain-directory leases still work but have no watcher coverage.
                if !watched.is_empty() {
                    tokio::time::sleep(FLUSH_GRACE).await;
                }
                while let Ok(event) = rx.try_recv() {
                    if pending.len() < 4096 { pending.push(event); }
                    else { gap.store(true, Ordering::Relaxed); }
                }
                drain(&daemon, &mut pending, &watched, &gap).await;
                let _ = ack.send(());
            }
            _ = flush.tick() => {
                drain(&daemon, &mut pending, &watched, &gap).await;
            }
        }
    }
}

/// Record everything pending, and report a gap if the OS or the queue
/// dropped events since the last flush.
async fn drain(
    daemon: &Daemon,
    pending: &mut Vec<notify::Event>,
    watched: &BTreeMap<PathBuf, Watched>,
    gap: &AtomicBool,
) {
    if gap.swap(false, Ordering::Relaxed) {
        daemon.emit(agentdocker_core::EventKind::WatcherGap {
            reason: "event overflow or operating-system watcher error; ledger may be incomplete"
                .into(),
        });
    }
    if pending.is_empty() {
        return;
    }
    let batch = std::mem::take(pending);
    let (observed, vcs_touched) = classify(&batch, watched);
    if !observed.is_empty() || !vcs_touched.is_empty() {
        daemon.record_fs_changes(observed, vcs_touched).await;
    }
}

fn reconcile_watches(
    daemon: &Daemon,
    watcher: &mut notify::RecommendedWatcher,
    watched: &mut BTreeMap<PathBuf, Watched>,
    retries: &mut HashMap<PathBuf, std::time::Instant>,
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
        if retries
            .get(&checkout.dir)
            .is_some_and(|at| at.elapsed() < Duration::from_secs(30))
        {
            continue;
        }
        if watched.contains_key(&checkout.dir) {
            continue;
        }
        if let Err(err) = watcher.watch(&checkout.dir, RecursiveMode::Recursive) {
            warn!(checkout = %checkout.dir.display(), %err, "cannot watch checkout");
            retries.insert(checkout.dir.clone(), std::time::Instant::now());
            daemon.emit(agentdocker_core::EventKind::WatcherGap {
                reason: format!(
                    "cannot watch {}: {err}; retrying in 30 seconds",
                    checkout.dir.display()
                ),
            });
            continue;
        }
        // A linked worktree keeps HEAD in its own git directory, elsewhere.
        let own_git = checkout.dir.join(".git");
        let gitdir = vcs::git_dirs(&checkout.dir)
            .map(|(gitdir, _)| gitdir)
            .filter(|gitdir| *gitdir != own_git)
            .filter(|gitdir| watcher.watch(gitdir, RecursiveMode::NonRecursive).is_ok());
        info!(checkout = %checkout.dir.display(), project = %checkout.project.short(), "watching");
        watched.insert(checkout.dir.clone(), Watched { checkout, gitdir });
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
            let is_dir = matches!(
                event.kind,
                EventKind::Remove(notify::event::RemoveKind::Folder)
            ) || path.is_dir();
            if is_dir && kind != ChangeKind::Removed {
                continue;
            }
            if ignored(dir, path, is_dir) {
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

/// Evaluate global, repository and nested ignore rules with their own roots.
/// Rebuilding on demand makes edits to ignore sources effective immediately.
fn ignored(root: &Path, path: &Path, is_dir: bool) -> bool {
    let (global, _) = GitignoreBuilder::new(root).build_global();
    let mut matchers = vec![global];
    if let Some((_, common)) = vcs::git_dirs(root) {
        let mut builder = GitignoreBuilder::new(root);
        let _ = builder.add(common.join("info/exclude"));
        if let Ok(matcher) = builder.build() {
            matchers.push(matcher);
        }
    }
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let mut current = root.to_path_buf();
    let parts: Vec<_> = relative.components().collect();
    for (index, part) in parts.iter().enumerate() {
        let (matcher, _) = Gitignore::new(current.join(".gitignore"));
        matchers.push(matcher);
        current.push(part.as_os_str());
        let directory = index + 1 < parts.len() || is_dir;
        let mut ignored = false;
        for matcher in &matchers {
            let matched = matcher.matched(&current, directory);
            if !matched.is_none() {
                ignored = matched.is_ignore();
            }
        }
        if ignored {
            return true;
        }
    }
    false
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
        .max_by_key(|w| {
            let root = if path.starts_with(&w.checkout.dir) {
                w.checkout.dir.as_os_str().len()
            } else {
                0
            };
            let git = w
                .gitdir
                .as_ref()
                .filter(|g| path.starts_with(g))
                .map_or(0, |g| g.as_os_str().len());
            root.max(git)
        })
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
    fn removed_directories_keep_their_type_for_ignore_rules() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        for name in ["target", "src"] {
            std::fs::create_dir(root.join(name)).unwrap();
            std::fs::remove_dir(root.join(name)).unwrap();
        }
        let watched = BTreeMap::from([(
            root.clone(),
            Watched {
                checkout: Checkout {
                    dir: root.clone(),
                    project: agentdocker_core::ProjectId::from("test"),
                    worktree: None,
                },
                gitdir: None,
            },
        )]);
        let events = ["target", "src"].map(|name| {
            notify::Event::new(EventKind::Remove(notify::event::RemoveKind::Folder))
                .add_path(root.join(name))
        });
        let (observed, vcs) = classify(&events, &watched);
        assert!(vcs.is_empty());
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].path, Path::new("src"));
        assert_eq!(observed[0].kind, ChangeKind::Removed);
    }

    #[test]
    fn nested_ignore_changes_take_effect_without_restarting_watch() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("src")).unwrap();
        let path = root.join("src/generated");
        assert!(!ignored(root, &path, false));
        std::fs::write(root.join("src/.gitignore"), "/generated\n").unwrap();
        assert!(ignored(root, &path, false));
        std::fs::write(root.join("src/.gitignore"), "").unwrap();
        assert!(!ignored(root, &path, false));
    }

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
