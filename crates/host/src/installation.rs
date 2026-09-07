//! Lifetime protection for binaries in a managed desktop installation.
//!
//! Pin files are stable outside version directories and are never deleted by
//! retention. An exclusive maintenance lock and shared running binaries use
//! the same inode; a startup losing that race exits before using the payload.
use crate::{dirs, lock};
use std::io;
use std::path::{Path, PathBuf};

pub const LOCK_FORMAT: u32 = 1;

pub fn pin_current_executable() -> io::Result<Option<lock::Lock>> {
    pin_executable(&std::env::current_exe()?)
}

/// Locate only our versioned layout. An ordinary checkout/package is unpinned.
fn managed(executable: &Path) -> Option<(PathBuf, PathBuf, &str)> {
    let versions = executable
        .ancestors()
        .find(|path| path.ends_with(".local/share/agentdocker/desktop/versions"))?;
    let relative = executable.strip_prefix(versions).ok()?;
    let id = relative.components().next()?.as_os_str().to_str()?;
    Some((versions.parent()?.to_owned(), versions.join(id), id))
}

pub fn pin_path(root: &Path, id: &str) -> io::Result<PathBuf> {
    if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::other("invalid desktop release identity"));
    }
    Ok(root.join("pins").join(format!("{id}.lock")))
}

fn pin_executable(executable: &Path) -> io::Result<Option<lock::Lock>> {
    let Some((root, version, id)) = managed(executable) else {
        return Ok(None);
    };
    let path = pin_path(&root, id)?;
    dirs::secure_state_dir(&root)?;
    dirs::secure_state_dir(&root.join("pins"))?;
    let pin = lock::try_shared(&path)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "desktop release is being removed; reopen the active installation",
        )
    })?;
    // A pruner may have won before this lock was opened. Pin files outlive
    // deletion, so startup must recheck the version while holding the pin.
    if !version.is_dir() || !executable.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "desktop release was removed; reopen the active installation",
        ));
    }
    Ok(Some(pin))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join(".local/share/agentdocker/desktop");
        let executable = root
            .join("versions")
            .join("a".repeat(64))
            .join("payload/agentdocker");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, "fixture").unwrap();
        (temp, root, executable)
    }

    #[test]
    fn running_binaries_share_one_pin_and_block_retention() {
        let (_temp, root, exe) = fixture();
        let first = pin_executable(&exe).unwrap().unwrap();
        let second = pin_executable(&exe).unwrap().unwrap();
        let path = pin_path(&root, &"a".repeat(64)).unwrap();
        assert!(lock::try_exclusive(&path).unwrap().is_none());
        drop(first);
        assert!(lock::try_exclusive(&path).unwrap().is_none());
        drop(second);
        assert!(lock::try_exclusive(&path).unwrap().is_some());
    }

    #[test]
    fn maintenance_blocks_startup_and_deleted_versions_cannot_rejoin() {
        let (_temp, root, exe) = fixture();
        drop(pin_executable(&exe).unwrap());
        let path = pin_path(&root, &"a".repeat(64)).unwrap();
        let held = lock::try_exclusive(&path).unwrap().unwrap();
        assert_eq!(
            pin_executable(&exe).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        std::fs::remove_file(&exe).unwrap();
        drop(held);
        assert_eq!(
            pin_executable(&exe).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert!(path.exists(), "pin inode remains stable after removal");
    }

    #[test]
    fn ordinary_executable_does_not_create_installation_state() {
        let temp = tempfile::tempdir().unwrap();
        assert!(
            pin_executable(&temp.path().join("agentdocker"))
                .unwrap()
                .is_none()
        );
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
        assert!(pin_path(temp.path(), "../outside").is_err());
    }
}
