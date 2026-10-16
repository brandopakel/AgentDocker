//! Folders the person took off AgentDocker's list. Its hooks leave the
//! sessions working there alone: no read-before-write guard, no leases, no
//! context put in front of the model. The desktop writes the list whenever
//! its own list of removed folders changes (a project removed, or added
//! back), and every hook reads it before anything else, without asking the
//! daemon, so a daemon that is old or down cannot keep a removed folder
//! coordinated.
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::path::{Path, PathBuf};

const FILE: &str = "uncoordinated.json";
/// A list larger than this is not one the desktop wrote; it is ignored.
const MAX_BYTES: u64 = 1 << 20;

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    roots: Vec<PathBuf>,
}

/// The folders taken off the list, as last written; none when the file is
/// missing, oversized or unreadable.
pub fn roots(home: &Path) -> Vec<PathBuf> {
    let path = home.join(FILE);
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() && meta.len() <= MAX_BYTES => {}
        _ => return Vec::new(),
    }
    std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).ok())
        .map(|saved| saved.roots)
        .unwrap_or_default()
}

/// Replace the list; an empty one removes the file.
pub fn write(home: &Path, roots: &[PathBuf]) -> std::io::Result<()> {
    let path = home.join(FILE);
    if roots.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    }
    let bytes = serde_json::to_vec_pretty(&Saved {
        roots: roots.to_vec(),
    })?;
    let mut tmp = tempfile::NamedTempFile::new_in(home)?;
    tmp.write_all(&bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(&path).map_err(|error| error.error)?;
    Ok(())
}

/// Whether `dir` is one of the folders taken off the list, or inside one.
/// Both sides are compared as the filesystem resolves them, so `/tmp` and
/// `/private/tmp` are one folder.
pub fn excluded(home: &Path, dir: &Path) -> bool {
    let roots = roots(home);
    if roots.is_empty() {
        return false;
    }
    let resolve = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let dir = resolve(dir);
    roots.iter().any(|root| dir.starts_with(resolve(root)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A removed folder and everything in it is excluded until it is added
    /// back; an unreadable list excludes nothing.
    #[test]
    fn a_removed_folder_and_its_subfolders_are_left_alone_until_added_back() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let lab = work.path().join("minio-lab");
        let other = work.path().join("other");
        std::fs::create_dir_all(lab.join("reproductions/e2e")).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        assert!(!excluded(home.path(), &lab));
        write(home.path(), &[lab.clone()]).unwrap();
        assert!(excluded(home.path(), &lab));
        assert!(excluded(home.path(), &lab.join("reproductions/e2e")));
        assert!(!excluded(home.path(), &other));
        write(home.path(), &[]).unwrap();
        assert!(!home.path().join(FILE).exists());
        assert!(!excluded(home.path(), &lab));
        std::fs::write(home.path().join(FILE), "not json").unwrap();
        assert!(!excluded(home.path(), &lab), "unreadable excludes nothing");
    }
}
