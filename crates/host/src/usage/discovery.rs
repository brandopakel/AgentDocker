//! Incremental, bounded discovery. An unfinished walk never means empty history.
//! Directory handles stay outside the daemon lock and are dropped with the walk.

use super::reader::Runtime;
use serde::{Deserialize, Serialize};
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
const PASS_ENTRIES: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Root {
    pub runtime: Runtime,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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

struct Directory {
    runtime: Runtime,
    depth: usize,
    entries: ReadDir,
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
        if roots.len() > MAX_ROOTS || roots.iter().any(|root| !root.path.is_absolute()) {
            return Err("usage discovery requires at most sixteen absolute roots");
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
                        match fs::read_dir(root.path) {
                            Ok(entries) => self.stack.push(Directory {
                                runtime: root.runtime,
                                depth: 0,
                                entries,
                            }),
                            Err(_) => self.gap(&mut page, "usage root cannot be enumerated"),
                        }
                    }
                    _ => self.gap(&mut page, "usage root is not a readable real directory"),
                }
                continue;
            }
            let directory = self.stack.last_mut().expect("directory present");
            let runtime = directory.runtime;
            let depth = directory.depth;
            let Some(entry) = directory.entries.next() else {
                self.stack.pop();
                continue;
            };
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
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(_) => {
                    self.gap(&mut page, "usage entry type cannot be read");
                    continue;
                }
            };
            if kind.is_dir() {
                if depth + 1 >= MAX_DEPTH {
                    self.gap(&mut page, "usage discovery depth limit reached");
                    continue;
                }
                match fs::read_dir(entry.path()) {
                    Ok(entries) => self.stack.push(Directory {
                        runtime,
                        depth: depth + 1,
                        entries,
                    }),
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
