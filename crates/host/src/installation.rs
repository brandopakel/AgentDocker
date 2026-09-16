//! Lifetime protection for binaries in a managed desktop installation.
//!
//! Pin files are stable outside version directories and are never deleted by
//! retention. An exclusive maintenance lock and shared running binaries use
//! the same inode; a startup losing that race exits before using the payload.
use crate::{dirs, lock};
use std::io;
use std::path::{Path, PathBuf};

pub const LOCK_FORMAT: u32 = 1;
pub const LAUNCHER_REDIRECT_FORMAT: u32 = 1;

/// A visible macOS application is an intact signed copy. Its entrypoints run
/// the selected immutable release before parsing commands or starting services.
/// Ownership is outside the signed bundle, in the installer's existing record.
pub fn redirect_managed_launcher() -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::process::CommandExt;
        let executable = crate::procinfo::executable_path()?;
        if let Some(target) = launcher_target(&executable, std::env::home_dir().as_deref())? {
            let _pin = pin_executable(&target)?.ok_or_else(|| {
                io::Error::other("launcher target is not an immutable installed release")
            })?;
            // exec preserves the invocation's terminal, arguments and process
            // identity. The selected executable acquires its own lifetime pin.
            return Err(std::process::Command::new(target)
                .args(std::env::args_os().skip(1))
                .exec());
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn launcher_target(executable: &Path, home: Option<&Path>) -> io::Result<Option<PathBuf>> {
    use std::io::Read;
    let Some(binary) = executable.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    if !matches!(binary, "agentdocker" | "agentd" | "agentdocker-ui") {
        return Ok(None);
    }
    let Some(macos) = executable.parent() else {
        return Ok(None);
    };
    let Some(contents) = macos.parent() else {
        return Ok(None);
    };
    let Some(application) = contents.parent() else {
        return Ok(None);
    };
    if macos.file_name() != Some(std::ffi::OsStr::new("MacOS"))
        || contents.file_name() != Some(std::ffi::OsStr::new("Contents"))
        || application.file_name() != Some(std::ffi::OsStr::new("AgentDocker.app"))
        || application.parent().and_then(Path::file_name)
            != Some(std::ffi::OsStr::new("Applications"))
    {
        return Ok(None);
    }
    let prefix = application.parent().and_then(Path::parent);
    for prefix in home.into_iter().chain(prefix) {
        let root = prefix.join(".local/share/agentdocker/desktop");
        let record = root.join("launcher.json");
        let metadata = match record.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if !metadata.is_file() || metadata.len() > 4096 {
            return Err(io::Error::other("invalid managed launcher record"));
        }
        let mut text = String::new();
        std::fs::File::open(&record)?
            .take(4097)
            .read_to_string(&mut text)?;
        if text.len() > 4096 {
            return Err(io::Error::other(
                "managed launcher record exceeds its bound",
            ));
        }
        let value: serde_json::Value = serde_json::from_str(&text)?;
        let matches = value["application"].as_str().is_some_and(|path| {
            Path::new(path).canonicalize().ok().as_deref() == Some(application)
        });
        if !matches {
            continue;
        }
        if value["format"] != 1 {
            return Err(io::Error::other("unknown managed launcher record format"));
        }
        let root = root.canonicalize()?;
        let target = root
            .join("current/payload/Contents/MacOS")
            .join(binary)
            .canonicalize()?;
        let Some((target_root, version, id)) = managed(&target) else {
            return Err(io::Error::other("launcher target escaped its installation"));
        };
        pin_path(&root, id)?;
        if target_root != root
            || target != version.join("AgentDocker.app/Contents/MacOS").join(binary)
        {
            return Err(io::Error::other("launcher target escaped its installation"));
        }
        return Ok(Some(target));
    }
    Ok(None)
}

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

pub fn pin_executable(executable: &Path) -> io::Result<Option<lock::Lock>> {
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

    #[cfg(unix)]
    fn launcher_fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let prefix = temp.path().canonicalize().unwrap();
        let root = prefix.join(".local/share/agentdocker/desktop");
        dirs::secure_state_dir(&root).unwrap();
        let application = prefix.join("Applications/AgentDocker.app");
        let launcher = application.join("Contents/MacOS/agentdocker");
        let payload = root
            .join("versions")
            .join("a".repeat(64))
            .join("AgentDocker.app");
        let selected = payload.join("Contents/MacOS/agentdocker");
        for file in [&launcher, &selected] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "fixture").unwrap();
        }
        std::fs::create_dir_all(root.join("generations/first")).unwrap();
        symlink(payload, root.join("generations/first/payload")).unwrap();
        symlink(root.join("generations/first"), root.join("current")).unwrap();
        std::fs::write(
            root.join("launcher.json"),
            serde_json::json!({
                "format": 1, "application": application,
            })
            .to_string(),
        )
        .unwrap();
        (temp, root, launcher, selected)
    }

    #[cfg(unix)]
    #[test]
    fn only_recorded_launchers_forward_to_an_exact_immutable_entrypoint() {
        use std::os::unix::fs::symlink;
        let (_temp, root, launcher, selected) = launcher_fixture();
        assert_eq!(
            launcher_target(&launcher, None).unwrap(),
            Some(selected.clone())
        );
        assert!(
            launcher_target(&selected, None).unwrap().is_none(),
            "no redirect loop"
        );
        assert!(
            launcher_target(&launcher.with_file_name("another"), None)
                .unwrap()
                .is_none()
        );
        let record = std::fs::read(root.join("launcher.json")).unwrap();
        std::fs::remove_file(root.join("launcher.json")).unwrap();
        assert!(
            launcher_target(&launcher, None).unwrap().is_none(),
            "unmanaged bundles run normally"
        );
        std::fs::write(root.join("launcher.json"), record).unwrap();
        let escaped = root.join("outside");
        std::fs::write(&escaped, "unrelated executable").unwrap();
        std::fs::remove_file(&selected).unwrap();
        symlink(escaped, &selected).unwrap();
        assert!(
            launcher_target(&launcher, None).is_err(),
            "selected executable cannot escape the store"
        );
        std::fs::write(root.join("launcher.json"), " ".repeat(4097)).unwrap();
        assert!(
            launcher_target(&launcher, None).is_err(),
            "bounded ownership record"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "owned subprocess invoked by copied_launcher_executes_and_pins_the_selected_release"]
    fn launcher_exec_fixture() {
        redirect_managed_launcher().unwrap();
        let root = std::env::current_dir()
            .unwrap()
            .join(".local/share/agentdocker/desktop");
        let expected = root
            .join("versions")
            .join("a".repeat(64))
            .join("AgentDocker.app/Contents/MacOS/agentdocker");
        assert_eq!(crate::procinfo::executable_path().unwrap(), expected);
        let _pin = pin_current_executable()
            .unwrap()
            .expect("selected release pinned");
        assert!(
            lock::try_exclusive(&pin_path(&root, &"a".repeat(64)).unwrap())
                .unwrap()
                .is_none()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn copied_launcher_executes_and_pins_the_selected_release() {
        let (temp, _root, launcher, selected) = launcher_fixture();
        for file in [&launcher, &selected] {
            std::fs::copy(crate::procinfo::executable_path().unwrap(), file).unwrap();
        }
        let output = crate::command::run(
            temp.path(),
            &[
                launcher.to_str().unwrap().into(),
                "--exact".into(),
                "installation::tests::launcher_exec_fixture".into(),
                "--ignored".into(),
                "--nocapture".into(),
            ],
            std::time::Duration::from_secs(10),
        )
        .unwrap();
        assert!(output.success, "{}", output.text);
    }

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
        // Match installation's private-state creation. On an elevated Windows
        // runner, ordinary create_dir_all can assign Administrators ownership,
        // which is intentionally refused for the final application state.
        dirs::secure_state_dir(&root).unwrap();
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
