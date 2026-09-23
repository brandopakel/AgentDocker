//! Incremental, bounded discovery. An unfinished walk never means empty history.
//! Directory handles stay outside the daemon lock and are dropped with the walk.

use super::reader::Runtime;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fs::{self, ReadDir},
    path::PathBuf,
    time::{Duration, Instant},
};

pub const MAX_ROOTS: usize = 16;
pub const MAX_FILES: usize = 10_000;
const MAX_ENTRIES: usize = 100_000;
const MAX_DEPTH: usize = 32;
pub const PASS_ENTRIES: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Root {
    pub runtime: Runtime,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub runtime: Runtime,
    pub path: PathBuf,
}

#[derive(Debug, Default)]
pub struct Page {
    pub sources: Vec<Source>,
    /// Fixed reasons, never file contents or OS error strings.
    pub gaps: Vec<&'static str>,
    pub finished: bool,
    pub complete: bool,
}

/// A durable frontier contains paths and directory-entry fingerprints only.
/// Restoring it replays each open directory's accepted prefix before advancing;
/// an OS enumeration order change refuses coverage rather than skipping names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    version: u32,
    roots: VecDeque<Root>,
    stack: Vec<DirectoryCheckpoint>,
    entries: usize,
    files: usize,
    incomplete: bool,
    exhausted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct DirectoryCheckpoint {
    runtime: Runtime,
    depth: usize,
    path: PathBuf,
    seen: usize,
    prefix: [u8; 32],
}

struct Directory {
    state: DirectoryCheckpoint,
    entries: Option<ReadDir>,
    replay: Option<DirectoryCheckpoint>,
}

fn valid_path(path: &std::path::Path) -> bool {
    path.is_absolute()
        && path.to_str().is_some_and(|p| p.len() <= 8192)
        && !path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
}

impl Directory {
    fn open(path: PathBuf, runtime: Runtime, depth: usize) -> std::io::Result<Self> {
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(std::io::Error::other("not a real directory"));
        }
        let entries = fs::read_dir(&path)?;
        Ok(Self {
            state: DirectoryCheckpoint {
                runtime,
                depth,
                path,
                seen: 0,
                prefix: [0; 32],
            },
            entries: Some(entries),
            replay: None,
        })
    }

    fn remember(&mut self, entry: &std::io::Result<fs::DirEntry>, kind: Option<&fs::FileType>) {
        let mut hash = Sha256::new();
        hash.update(self.state.prefix);
        match entry {
            Ok(entry) => {
                hash.update([0]);
                hash.update(entry.file_name().as_encoded_bytes());
                hash.update([match kind {
                    Some(k) if k.is_file() => 1,
                    Some(k) if k.is_dir() => 2,
                    Some(k) if k.is_symlink() => 3,
                    Some(_) => 4,
                    None => 5,
                }]);
            }
            Err(_) => hash.update([1]),
        }
        self.state.prefix = hash.finalize().into();
        self.state.seen += 1;
    }
}

pub struct Walk {
    roots: VecDeque<Root>,
    stack: Vec<Directory>,
    entries: usize,
    files: usize,
    incomplete: bool,
    exhausted: bool,
}

impl Walk {
    pub fn new(roots: Vec<Root>) -> Result<Self, &'static str> {
        if roots.len() > MAX_ROOTS {
            return Err(
                "usage discovery supports at most sixteen roots, including provider defaults",
            );
        }
        for root in &roots {
            if !root.path.is_absolute() {
                return Err("usage discovery roots must be absolute paths");
            }
            if root.path.components().any(|part| {
                matches!(
                    part,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            }) {
                return Err(
                    "usage discovery roots must not contain parent ('..') components; use a direct absolute path",
                );
            }
            if !valid_path(&root.path) {
                return Err("usage discovery root paths must be UTF-8 and at most 8192 bytes");
            }
        }
        Ok(Self {
            roots: roots.into(),
            stack: Vec::new(),
            entries: 0,
            files: 0,
            incomplete: false,
            exhausted: false,
        })
    }

    pub fn finished(&self) -> bool {
        self.exhausted
    }

    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            version: 1,
            roots: self.roots.clone(),
            stack: self
                .stack
                .iter()
                .map(|d| d.replay.as_ref().unwrap_or(&d.state).clone())
                .collect(),
            entries: self.entries,
            files: self.files,
            incomplete: self.incomplete,
            exhausted: self.exhausted,
        }
    }

    pub fn resume(saved: Checkpoint, roots: &[Root]) -> Result<Self, &'static str> {
        let Some(started) = roots.len().checked_sub(saved.roots.len()) else {
            return Err("usage discovery checkpoint has unrelated roots");
        };
        if roots.len() > MAX_ROOTS
            || roots.iter().any(|r| !valid_path(&r.path))
            || !roots[started..].iter().eq(saved.roots.iter())
            || saved.stack.first().is_some_and(|directory| {
                started
                    .checked_sub(1)
                    .and_then(|index| roots.get(index))
                    .is_none_or(|root| {
                        root.path != directory.path || root.runtime != directory.runtime
                    })
            })
        {
            return Err("usage discovery checkpoint has unrelated roots");
        }
        if saved.version != 1
            || saved.roots.len() > MAX_ROOTS
            || saved.stack.len() > MAX_DEPTH
            || saved.entries > MAX_ENTRIES + 1
            || saved.files > MAX_FILES + 1
            || saved.roots.iter().any(|r| !valid_path(&r.path))
            || saved
                .stack
                .iter()
                .any(|d| !valid_path(&d.path) || d.seen > saved.entries || d.depth >= MAX_DEPTH)
            || saved.stack.first().is_some_and(|d| d.depth != 0)
            || saved.stack.windows(2).any(|pair| {
                pair[1].path.parent() != Some(pair[0].path.as_path())
                    || pair[1].depth != pair[0].depth + 1
                    || pair[1].runtime != pair[0].runtime
            })
            || (saved.exhausted && (!saved.roots.is_empty() || !saved.stack.is_empty()))
        {
            return Err("invalid usage discovery checkpoint");
        }
        Ok(Self {
            roots: saved.roots,
            stack: saved
                .stack
                .into_iter()
                .map(|d| Directory {
                    state: DirectoryCheckpoint {
                        seen: 0,
                        prefix: [0; 32],
                        ..d.clone()
                    },
                    entries: None,
                    replay: Some(d),
                })
                .collect(),
            entries: saved.entries,
            files: saved.files,
            incomplete: saved.incomplete,
            exhausted: saved.exhausted,
        })
    }

    /// At most 512 entries and a cooperative 25 ms per call. Limits on total
    /// entries/files/depth end this snapshot as incomplete, rather than claiming
    /// all history was scanned. A new generation can retry changed directories.
    pub fn next_page(&mut self) -> Page {
        let mut page = Page::default();
        let start = Instant::now();
        for _ in 0..PASS_ENTRIES {
            if self.exhausted || start.elapsed() >= Duration::from_millis(25) {
                break;
            }
            if self.stack.is_empty() {
                let Some(root) = self.roots.pop_front() else {
                    self.exhausted = true;
                    break;
                };
                match fs::symlink_metadata(&root.path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
                        match Directory::open(root.path, root.runtime, 0) {
                            Ok(directory) => self.stack.push(directory),
                            Err(_) => self.gap(&mut page, "usage root cannot be enumerated"),
                        }
                    }
                    _ => self.gap(&mut page, "usage root is not a readable real directory"),
                }
                continue;
            }
            // Verify ancestors before opening a retained child path. A replaced
            // or redirected root must refuse before child enumeration resumes.
            let last = self.stack.len() - 1;
            let index = self
                .stack
                .iter()
                .position(|d| d.replay.is_some())
                .unwrap_or(last);
            let directory = &mut self.stack[index];
            if directory.entries.is_none() {
                match Directory::open(
                    directory.state.path.clone(),
                    directory.state.runtime,
                    directory.state.depth,
                ) {
                    Ok(opened) => directory.entries = opened.entries,
                    Err(_) => {
                        self.gap(&mut page, "usage discovery directory cannot be resumed");
                        self.finish();
                        break;
                    }
                }
            }
            if let Some(replay) = &directory.replay
                && directory.state.seen == replay.seen
            {
                if directory.state.prefix != replay.prefix {
                    self.gap(&mut page, "usage discovery directory prefix changed");
                    self.finish();
                    break;
                }
                directory.replay = None;
                if index != last {
                    continue;
                }
            }
            let runtime = directory.state.runtime;
            let depth = directory.state.depth;
            let Some(entry) = directory.entries.as_mut().expect("directory opened").next() else {
                if directory.replay.is_some() {
                    self.gap(&mut page, "usage discovery directory prefix changed");
                    self.finish();
                    break;
                }
                self.stack.pop();
                continue;
            };
            let kind = entry.as_ref().ok().and_then(|entry| entry.file_type().ok());
            directory.remember(&entry, kind.as_ref());
            if directory.replay.is_some() {
                // Rechecking old entries consumes this page's work allowance,
                // never another source or global entry allowance.
                continue;
            }
            self.entries += 1;
            if self.entries > MAX_ENTRIES {
                self.gap(&mut page, "usage discovery entry limit reached");
                self.finish();
                break;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    self.gap(&mut page, "usage directory entry cannot be read");
                    continue;
                }
            };
            let kind = match kind {
                Some(kind) => kind,
                None => {
                    self.gap(&mut page, "usage entry type cannot be read");
                    continue;
                }
            };
            if kind.is_dir() {
                if depth + 1 >= MAX_DEPTH {
                    self.gap(&mut page, "usage discovery depth limit reached");
                    continue;
                }
                if !valid_path(&entry.path()) {
                    self.gap(
                        &mut page,
                        "usage path is not representable within its limit",
                    );
                    continue;
                }
                match Directory::open(entry.path(), runtime, depth + 1) {
                    Ok(directory) => self.stack.push(directory),
                    Err(_) => self.gap(&mut page, "usage directory cannot be enumerated"),
                }
            } else if entry.path().extension().is_some_and(|ext| ext == "jsonl") {
                if !kind.is_file() {
                    self.gap(&mut page, "usage source is not a regular file");
                    continue;
                }
                if entry.path().to_str().is_none_or(|path| path.len() > 8192) {
                    self.gap(
                        &mut page,
                        "usage path is not representable within its limit",
                    );
                    continue;
                }
                self.files += 1;
                if self.files > MAX_FILES {
                    self.gap(&mut page, "usage discovery file limit reached");
                    self.finish();
                    break;
                }
                page.sources.push(Source {
                    runtime,
                    path: entry.path(),
                });
            }
        }
        page.finished = self.exhausted;
        page.complete = self.exhausted && !self.incomplete;
        page
    }

    fn gap(&mut self, page: &mut Page, reason: &'static str) {
        self.incomplete = true;
        if !page.gaps.contains(&reason) {
            page.gaps.push(reason);
        }
    }

    fn finish(&mut self) {
        self.exhausted = true;
        self.stack.clear();
        self.roots.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialized_frontier_resumes_without_reemitting_a_verified_prefix() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("nested");
        fs::create_dir(&nested).unwrap();
        for n in 0..900 {
            fs::write(nested.join(format!("{n}.jsonl")), "").unwrap();
        }
        let roots = vec![Root {
            runtime: Runtime::Claude,
            path: temp.path().to_owned(),
        }];
        let mut walk = Walk::new(roots.clone()).unwrap();
        let first = walk.next_page();
        assert!(!first.finished);
        assert!(!first.sources.is_empty());
        let saved = serde_json::to_vec(&walk.checkpoint()).unwrap();
        drop(walk);
        let mut resumed = Walk::resume(serde_json::from_slice(&saved).unwrap(), &roots).unwrap();
        let mut seen: std::collections::BTreeSet<_> =
            first.sources.into_iter().map(|s| s.path).collect();
        loop {
            let page = resumed.next_page();
            for source in page.sources {
                assert!(
                    seen.insert(source.path),
                    "replayed directory entries must not reemit sources"
                );
            }
            if page.finished {
                assert!(page.complete, "{:?}", page.gaps);
                break;
            }
        }
        assert_eq!(seen.len(), 900);
    }

    #[test]
    fn changed_directory_prefix_refuses_completion_after_resume() {
        let temp = tempfile::tempdir().unwrap();
        for n in 0..600 {
            fs::write(temp.path().join(format!("{n}.jsonl")), "").unwrap();
        }
        let roots = vec![Root {
            runtime: Runtime::Codex,
            path: temp.path().to_owned(),
        }];
        let mut walk = Walk::new(roots.clone()).unwrap();
        let first = walk.next_page();
        let saved = walk.checkpoint();
        fs::remove_file(&first.sources[0].path).unwrap();
        let mut resumed = Walk::resume(saved, &roots).unwrap();
        loop {
            let page = resumed.next_page();
            assert!(page.sources.is_empty());
            if page.finished {
                assert!(!page.complete);
                assert!(
                    page.gaps
                        .contains(&"usage discovery directory prefix changed")
                );
                break;
            }
        }
    }

    #[test]
    fn checkpoint_bounds_are_checked_before_opening_directories() {
        let temp = tempfile::tempdir().unwrap();
        let roots = vec![Root {
            runtime: Runtime::Claude,
            path: temp.path().to_owned(),
        }];
        let walk = Walk::new(roots.clone()).unwrap();
        let mut saved = walk.checkpoint();
        saved.entries = MAX_ENTRIES + 2;
        assert!(Walk::resume(saved, &roots).is_err());
        let mut saved = walk.checkpoint();
        saved.roots[0].path = PathBuf::from("relative");
        assert!(Walk::resume(saved, &roots).is_err());
    }

    #[test]
    fn resumed_frontier_is_bound_to_the_configured_root_order_and_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let current = temp.path().join("current");
        fs::create_dir(&current).unwrap();
        for n in 0..600 {
            fs::write(current.join(format!("{n}.jsonl")), "").unwrap();
        }
        let roots: Vec<_> = [
            temp.path().join("missing"),
            current.clone(),
            temp.path().join("later"),
        ]
        .into_iter()
        .map(|path| Root {
            runtime: Runtime::Claude,
            path,
        })
        .collect();
        let mut walk = Walk::new(roots.clone()).unwrap();
        assert!(!walk.next_page().finished);
        let saved = walk.checkpoint();
        assert_eq!(saved.roots.len(), 1);
        assert_eq!(saved.stack[0].path, current);
        assert!(Walk::resume(saved.clone(), &roots).is_ok());

        let mut changed = saved.clone();
        changed.stack[0].path = temp.path().to_owned();
        assert!(Walk::resume(changed, &roots).is_err());
        let mut changed = saved.clone();
        changed.stack[0].runtime = Runtime::Codex;
        assert!(Walk::resume(changed, &roots).is_err());
        let mut changed = saved.clone();
        changed.roots[0].path = temp.path().join("unconfigured");
        assert!(Walk::resume(changed, &roots).is_err());
        let mut changed = saved.clone();
        changed.stack.push(DirectoryCheckpoint {
            path: current.join(".."),
            depth: 1,
            ..changed.stack[0].clone()
        });
        assert!(Walk::resume(changed, &roots).is_err());
        assert!(
            Walk::new(vec![Root {
                runtime: Runtime::Claude,
                path: current.join("..")
            }])
            .is_err()
        );

        let mut resumed = Walk::resume(saved, &roots).unwrap();
        while !resumed.next_page().finished {}
        assert!(
            Walk::resume(resumed.checkpoint(), &roots)
                .unwrap()
                .finished()
        );
    }

    #[cfg(unix)]
    #[test]
    fn redirected_parent_refuses_before_resuming_the_same_child_directory() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("logs");
        let child = root.join("child");
        fs::create_dir_all(&child).unwrap();
        for n in 0..600 {
            fs::write(child.join(format!("{n}.jsonl")), "").unwrap();
        }
        let roots = vec![Root {
            runtime: Runtime::Claude,
            path: root.clone(),
        }];
        let mut walk = Walk::new(roots.clone()).unwrap();
        // A page may use its entire time budget opening the directories,
        // especially under coverage. Establish the checkpoint precondition
        // through bounded progress, without assuming one page emits a file.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let page = walk.next_page();
            assert!(!page.finished && page.gaps.is_empty());
            if !page.sources.is_empty() {
                break;
            }
            assert!(Instant::now() < deadline, "discovery made no file progress");
        }
        let saved = walk.checkpoint();
        let moved = temp.path().join("moved");
        fs::rename(&root, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &root).unwrap();
        let mut resumed = Walk::resume(saved, &roots).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let page = loop {
            let page = resumed.next_page();
            assert!(page.sources.is_empty());
            if page.finished {
                break page;
            }
            assert!(
                Instant::now() < deadline,
                "redirected directory was not refused"
            );
        };
        assert!(!page.complete);
        assert!(
            page.gaps
                .contains(&"usage discovery directory cannot be resumed")
        );
    }

    #[test]
    fn discovery_is_incremental_and_does_not_claim_special_files() {
        let temp = tempfile::tempdir().unwrap();
        for n in 0..600 {
            fs::write(temp.path().join(format!("{n}.jsonl")), "").unwrap();
        }
        fs::write(temp.path().join("ignored.txt"), "").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("0.jsonl", temp.path().join("link.jsonl")).unwrap();
        let mut walk = Walk::new(vec![Root {
            runtime: Runtime::Codex,
            path: temp.path().to_owned(),
        }])
        .unwrap();
        let first = walk.next_page();
        assert!(!first.finished);
        assert!(!first.complete);
        let mut paths: std::collections::BTreeSet<_> =
            first.sources.into_iter().map(|s| s.path).collect();
        let final_page = loop {
            let page = walk.next_page();
            paths.extend(page.sources.iter().map(|s| s.path.clone()));
            if page.finished {
                break page;
            }
        };
        assert_eq!(paths.len(), 600);
        assert_eq!(final_page.complete, !cfg!(unix));
        assert!(walk.next_page().finished);
    }

    #[test]
    fn missing_roots_are_empty_but_wrong_roots_leave_coverage_unknown() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        let mut walk = Walk::new(vec![Root {
            runtime: Runtime::Claude,
            path: missing,
        }])
        .unwrap();
        let page = walk.next_page();
        assert!(page.complete);
        let file = temp.path().join("file");
        fs::write(&file, "").unwrap();
        let mut walk = Walk::new(vec![Root {
            runtime: Runtime::Claude,
            path: file,
        }])
        .unwrap();
        let page = walk.next_page();
        assert!(page.finished);
        assert!(!page.complete);
        assert_eq!(page.gaps.len(), 1);
    }
}
