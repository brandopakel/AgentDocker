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
    pin_executable(&crate::procinfo::executable_path()?)
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

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    #[ignore = "owned subprocess invoked by launch_alias_pins_loaded_release_after_activation"]
    fn loaded_release_fixture() {
        use std::os::unix::fs::symlink;
        let root = std::env::current_dir().unwrap();
        let versions = root.join(".local/share/agentdocker/desktop/versions");
        let expected = versions.join("a".repeat(64)).join("payload/agentd");
        assert_eq!(crate::procinfo::executable_path().unwrap(), expected);
        let pin = pin_current_executable()
            .unwrap()
            .expect("loaded release is pinned");
        symlink(
            versions.join("b".repeat(64)).join("payload"),
            root.join("next"),
        )
        .unwrap();
        std::fs::rename(root.join("next"), root.join("current")).unwrap();
        assert_eq!(crate::procinfo::executable_path().unwrap(), expected);
        // Looking up the loaded release after activation must still pin A.
        let second_pin = pin_current_executable().unwrap().unwrap();
        let path = pin_path(versions.parent().unwrap(), &"a".repeat(64)).unwrap();
        assert!(lock::try_exclusive(&path).unwrap().is_none());
        drop(pin);
        assert!(lock::try_exclusive(&path).unwrap().is_none());
        drop(second_pin);
        assert!(lock::try_exclusive(&path).unwrap().is_some());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn launch_alias_pins_loaded_release_after_activation() {
        use std::os::unix::fs::symlink;
        let (temp, _root, executable) = fixture();
        let root = temp.path().canonicalize().unwrap();
        let loaded = executable.with_file_name("agentd");
        std::fs::copy(crate::procinfo::executable_path().unwrap(), &loaded).unwrap();
        let versions = loaded.parent().unwrap().parent().unwrap().parent().unwrap();
        let replacement = versions.join("b".repeat(64)).join("payload");
        std::fs::create_dir_all(&replacement).unwrap();
        std::fs::write(replacement.join("agentd"), "replacement generation").unwrap();
        symlink(loaded.parent().unwrap(), root.join("current")).unwrap();
        symlink(root.join("current/agentd"), root.join("launch")).unwrap();
        let output = crate::command::run(
            &root,
            &[
                root.join("launch").to_str().unwrap().into(),
                "--exact".into(),
                "installation::tests::loaded_release_fixture".into(),
                "--ignored".into(),
                "--nocapture".into(),
            ],
            std::time::Duration::from_secs(10),
        )
        .unwrap();
        assert!(output.success, "{}", output.text);
    }

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
