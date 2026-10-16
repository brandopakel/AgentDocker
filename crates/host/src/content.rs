//! Bounded, deterministic content snapshots for observations and validation.
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const MAX_FILES: usize = 20_000;
const MAX_BYTES: u64 = 256 * 1024 * 1024;

/// The fingerprint of a path with nothing there.
pub const MISSING: &str = "missing";

/// Hash a file or an ignore-aware directory tree. Refuse special files and
/// oversized trees; a failed observation must never become a fresh read mark.
pub fn fingerprint(path: &Path) -> io::Result<String> {
    Ok(snapshot(path)?.version)
}

/// The same bounded observation, retaining the individual directory entries.
/// Entries are relative paths; a file or an absent root has no entry map.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub version: String,
    pub entries: Option<BTreeMap<PathBuf, String>>,
}

impl Snapshot {
    /// One entry's identity, without confusing an absent child with its parent.
    pub fn entry(&self, relative: &Path) -> &str {
        if relative.as_os_str().is_empty() {
            return if self.entries.is_some() {
                "directory"
            } else {
                &self.version
            };
        }
        self.entries
            .as_ref()
            .and_then(|e| e.get(relative))
            .map_or(MISSING, String::as_str)
    }

    /// A conservative payload estimate for bounded ephemeral proof caches.
    pub fn retained_bytes(&self) -> usize {
        256 + self.version.len() * 2
            + self.entries.as_ref().map_or(0, |entries| {
                entries
                    .iter()
                    .map(|(path, version)| 256 + path.as_os_str().len() * 2 + version.len() * 2)
                    .sum::<usize>()
            })
    }

    /// Content differences under a requested path, including removals.
    /// A directory marker does not stand in for the contents of that directory.
    pub fn differences(&self, current: &Self, target: &Path) -> Vec<(PathBuf, String)> {
        let mut paths = std::collections::BTreeSet::new();
        paths.insert(Path::new(""));
        for entries in [&self.entries, &current.entries].into_iter().flatten() {
            paths.extend(entries.keys().map(PathBuf::as_path));
        }
        paths
            .into_iter()
            .filter(|path| path.starts_with(target) && self.entry(path) != current.entry(path))
            .map(|path| (path.to_path_buf(), current.entry(path).to_owned()))
            .collect()
    }
}

/// Hash only the entry itself. Watcher proofs never rescan a whole subtree.
pub fn entry_fingerprint(path: &Path, budget: &mut u64) -> io::Result<String> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(MISSING.into()),
        Err(e) => Err(e),
        Ok(metadata) if metadata.is_dir() => Ok("directory".into()),
        Ok(_) => file_digest(path, budget),
    }
}

pub fn snapshot(path: &Path) -> io::Result<Snapshot> {
    let mut entries = BTreeMap::new();
    let mut budget = MAX_BYTES;
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(Snapshot {
                version: MISSING.into(),
                entries: None,
            });
        }
        Err(e) => return Err(e),
        Ok(m) if !m.is_dir() => {
            return Ok(Snapshot {
                version: file_digest(path, &mut budget)?,
                entries: None,
            });
        }
        Ok(_) => {}
    }
    for entry in ignore::WalkBuilder::new(path)
        .hidden(false)
        .require_git(false)
        .follow_links(false)
        .build()
    {
        let entry = entry.map_err(io::Error::other)?;
        let relative = entry.path().strip_prefix(path).map_err(io::Error::other)?;
        if relative.as_os_str().is_empty() || relative.components().any(|c| c.as_os_str() == ".git")
        {
            continue;
        }
        if entries.len() >= MAX_FILES {
            return Err(io::Error::other("snapshot exceeds 20000 entries"));
        }
        let digest = if entry.file_type().is_some_and(|t| t.is_dir()) {
            "directory".into()
        } else {
            file_digest(entry.path(), &mut budget)?
        };
        entries.insert(PathBuf::from(relative), digest);
    }
    let mut hash = Sha256::new();
    for (path, digest) in &entries {
        let bytes = path.as_os_str().as_encoded_bytes();
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
        hash.update(digest.as_bytes());
    }
    Ok(Snapshot {
        version: format!("sha256:{:x}", hash.finalize()),
        entries: Some(entries),
    })
}

fn file_digest(path: &Path, budget: &mut u64) -> io::Result<String> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(path)?;
        return Ok(format!(
            "symlink:{:x}",
            Sha256::digest(target.as_os_str().as_encoded_bytes())
        ));
    }
    let mut file = crate::files::open_regular(path)?;
    let stamp = crate::files::stamp(&file)?;
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(io::Error::other("snapshot requires a regular file"));
    }
    // Past the hashing budget (a 500 MiB dataset in an observed folder),
    // the file is known by its length, modification time and mode instead:
    // a change to it still changes the fingerprint, and the rest of the
    // observation goes ahead rather than failing on one file.
    if before.len() > *budget {
        let modified = before
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        return Ok(format!(
            "large:{}:{modified}:{:o}",
            before.len(),
            crate::files::observed_permissions(&before)
        ));
    }
    let mut hash = Sha256::new();
    hash.update(crate::files::observed_permissions(&before).to_le_bytes());
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        *budget = budget
            .checked_sub(n as u64)
            .ok_or_else(|| io::Error::other("snapshot exceeds 256 MiB"))?;
        hash.update(&buffer[..n]);
    }
    if stamp != crate::files::stamp(&file)? {
        return Err(io::Error::other(
            "file changed while being observed; retry the read",
        ));
    }
    Ok(format!("sha256:{:x}", hash.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_distinguish_unseen_removed_and_unchanged_targets() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a"), "a").unwrap();
        std::fs::write(tmp.path().join("b"), "b").unwrap();
        let before = snapshot(tmp.path()).unwrap();
        assert_eq!(before.version, fingerprint(tmp.path()).unwrap());
        std::fs::write(tmp.path().join("b"), "changed").unwrap();
        std::fs::create_dir(tmp.path().join("new-dir")).unwrap();
        let after = snapshot(tmp.path()).unwrap();
        assert!(before.differences(&after, Path::new("a")).is_empty());
        assert!(
            before
                .differences(&after, Path::new("new-dir/unseen"))
                .is_empty()
        );
        std::fs::remove_file(tmp.path().join("a")).unwrap();
        let removed = snapshot(tmp.path()).unwrap();
        assert_eq!(
            before.differences(&removed, Path::new("a")),
            vec![(PathBuf::from("a"), MISSING.to_owned())]
        );
        let mut budget = 1;
        assert_eq!(
            entry_fingerprint(&tmp.path().join("a"), &mut budget).unwrap(),
            MISSING
        );
        assert_eq!(
            entry_fingerprint(&tmp.path().join("new-dir"), &mut budget).unwrap(),
            "directory"
        );
        assert!(
            entry_fingerprint(&tmp.path().join("b"), &mut budget)
                .unwrap()
                .starts_with("large:")
        );
        assert_eq!(budget, 1);
    }

    /// A file past the hashing budget is known by its stamp: the folder is
    /// still observed, and touching the file still changes it.
    #[test]
    fn a_file_past_the_budget_is_known_by_its_stamp() {
        let tmp = tempfile::tempdir().unwrap();
        let big = tmp.path().join("big.bin");
        std::fs::write(&big, vec![0u8; 64]).unwrap();
        let mut budget = 16;
        let first = file_digest(&big, &mut budget).unwrap();
        assert!(first.starts_with("large:64:"), "{first}");
        assert_eq!(budget, 16, "nothing was read");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&big, vec![1u8; 65]).unwrap();
        assert_ne!(file_digest(&big, &mut 16).unwrap(), first);
    }
    #[cfg(unix)]
    #[test]
    fn snapshots_apply_ignores_outside_git_and_track_executable_bits() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "ignored\n").unwrap();
        std::fs::write(tmp.path().join("source"), "source").unwrap();
        let before = fingerprint(tmp.path()).unwrap();
        std::fs::write(tmp.path().join("ignored"), "output").unwrap();
        assert_eq!(before, fingerprint(tmp.path()).unwrap());
        std::fs::create_dir(tmp.path().join(".git")).unwrap();
        std::fs::write(tmp.path().join(".git/HEAD"), "metadata").unwrap();
        assert_eq!(before, fingerprint(tmp.path()).unwrap());
        let file = tmp.path().join("source");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode ^ 0o100)).unwrap();
        assert_ne!(before, fingerprint(tmp.path()).unwrap());
        let fifo = tmp.path().join("pipe");
        let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: a valid nul-terminated path is passed to mkfifo.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(fingerprint(&fifo).is_err());
    }

    #[test]
    fn uncommitted_changes_and_deletions_change_content_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("file");
        std::fs::write(&path, "one").unwrap();
        let first = fingerprint(tmp.path()).unwrap();
        std::fs::write(&path, "two").unwrap();
        assert_ne!(first, fingerprint(tmp.path()).unwrap());
        std::fs::remove_file(&path).unwrap();
        assert_eq!(fingerprint(&path).unwrap(), "missing");
        assert_ne!(first, fingerprint(tmp.path()).unwrap());
    }
}
