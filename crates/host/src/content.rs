//! Bounded, deterministic content snapshots for observations and validation.
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const MAX_FILES: usize = 20_000;
const MAX_BYTES: u64 = 256 * 1024 * 1024;

/// Hash a file or an ignore-aware directory tree. Refuse special files and
/// oversized trees; a failed observation must never become a fresh read mark.
pub fn fingerprint(path: &Path) -> io::Result<String> {
    let mut entries = BTreeMap::new();
    let mut budget = MAX_BYTES;
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok("missing".into()),
        Err(e) => return Err(e),
        Ok(m) if !m.is_dir() => return file_digest(path, &mut budget),
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
    for (path, digest) in entries {
        let bytes = path.as_os_str().as_encoded_bytes();
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
        hash.update(digest.as_bytes());
    }
    Ok(format!("sha256:{:x}", hash.finalize()))
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
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)?;
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(io::Error::other("snapshot requires a regular file"));
    }
    if before.len() > *budget {
        return Err(io::Error::other("snapshot exceeds 256 MiB"));
    }
    let mut hash = Sha256::new();
    hash.update((before.mode() & 0o111).to_le_bytes());
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
    let after = file.metadata()?;
    if before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
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
