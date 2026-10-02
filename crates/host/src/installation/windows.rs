//! The Windows desktop store uses a private atomic record rather than a
//! symlink. Launchers and installed processes resolve only an exact immutable
//! executable inside that store; no path from a record is executed directly.
use crate::{dirs, files};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub const STORE_SUFFIX: &str = "AgentDocker/desktop";
pub const BINARIES: &[&str] = &["agentdocker.exe", "agentd.exe", "agentdocker-ui.exe"];

fn invalid(message: &'static str) -> io::Error {
    io::Error::other(message)
}

/// A path matches only the store's fixed, versioned executable layout.
pub fn managed(executable: &Path) -> Option<(PathBuf, PathBuf, &str)> {
    let binary = executable.file_name()?.to_str()?;
    if !BINARIES.contains(&binary) {
        return None;
    }
    let payload = executable.parent()?;
    if payload.file_name()? != "AgentDocker" {
        return None;
    }
    let version = payload.parent()?;
    let id = version.file_name()?.to_str()?;
    if !valid_id(id) {
        return None;
    }
    let versions = version.parent()?;
    if versions.file_name()? != "versions" {
        return None;
    }
    let root = versions.parent()?;
    if !root.ends_with(STORE_SUFFIX) {
        return None;
    }
    Some((root.to_owned(), version.to_owned(), id))
}

fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn record(path: &Path, bound: u64) -> io::Result<Value> {
    let file = dirs::open_private_snapshot(path)?;
    if file.metadata()?.len() > bound {
        return Err(invalid(
            "Windows installation record exceeds its size bound",
        ));
    }
    let mut bytes = Vec::new();
    file.take(bound + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > bound {
        return Err(invalid(
            "Windows installation record exceeds its size bound",
        ));
    }
    let value: Value = serde_json::from_slice(&bytes)?;
    if value["format"] != 1 {
        return Err(invalid("unknown Windows installation record format"));
    }
    Ok(value)
}

/// The current entrypoint, read from one complete activation record. The
/// caller takes the release pin before starting it and rechecks existence.
pub fn target(root: &Path, binary: &str) -> io::Result<Option<PathBuf>> {
    if !root.is_absolute() || !root.ends_with(STORE_SUFFIX) || !BINARIES.contains(&binary) {
        return Err(invalid("invalid Windows installation path or entrypoint"));
    }
    match dirs::check_private_dir(root) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let activation = match record(&root.join("activation.json"), 64 * 1024) {
        Ok(activation) => activation,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let root = root.canonicalize()?;
    if activation["inactive"] == true {
        if activation.get("current") == Some(&Value::Null) {
            return Ok(None);
        }
        return Err(invalid(
            "inactive installation still names a current release",
        ));
    }
    let current = &activation["current"];
    let id = current["id"]
        .as_str()
        .filter(|id| valid_id(id))
        .ok_or_else(|| invalid("invalid Windows activation release identity"))?;
    if current["payload"] != "AgentDocker" || current["installation_lock"] != 1 {
        return Err(invalid(
            "unsupported Windows activation payload or lifetime pin contract",
        ));
    }
    let expected = root
        .join("versions")
        .join(id)
        .join("AgentDocker")
        .join(binary);
    let target = expected.canonicalize()?;
    if target != expected {
        return Err(invalid("Windows activation escaped its immutable payload"));
    }
    // Reject a redirected/special final file as well as redirected parents.
    files::open_regular(&target)?;
    Ok(Some(target))
}

fn hash(path: &Path) -> io::Result<String> {
    let mut file = files::open_regular(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// A stable bootstrap belongs to this store only while its receipt and bytes
/// agree. Missing/damaged records in the reserved launcher directory fail
/// closed; an ordinary extracted portable executable is unaffected.
pub fn launcher_root(executable: &Path) -> io::Result<Option<PathBuf>> {
    let Some(binary) = executable.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    if !BINARIES.contains(&binary) {
        return Ok(None);
    }
    let Some(bin) = executable.parent() else {
        return Ok(None);
    };
    let Some(root) = bin.parent() else {
        return Ok(None);
    };
    if bin.file_name() != Some(std::ffi::OsStr::new("bin")) || !root.ends_with(STORE_SUFFIX) {
        return Ok(None);
    }
    dirs::check_private_dir(root)?;
    let receipt = record(&root.join("launcher.json"), 4096)?;
    let expected = receipt["binary_sha256"][binary]
        .as_str()
        .filter(|id| valid_id(id))
        .ok_or_else(|| invalid("Windows launcher receipt lacks its executable hash"))?;
    if hash(executable)? != expected {
        return Err(invalid("Windows launcher was modified"));
    }
    let root = root.canonicalize()?;
    if executable.canonicalize()? != root.join("bin").join(binary) {
        return Err(invalid("Windows launcher escaped its installation"));
    }
    Ok(Some(root))
}

pub fn launcher_target(executable: &Path) -> io::Result<Option<PathBuf>> {
    let Some(root) = launcher_root(executable)? else {
        return Ok(None);
    };
    let binary = executable
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("invalid launcher name"))?;
    target(&root, binary)?
        .map(Some)
        .ok_or_else(|| invalid("Windows installation is inactive"))
}

/// Stable registration path for a managed executable. Obsolete running copies
/// must reopen before changing provider or service registrations.
pub fn stable_executable(executable: &Path) -> io::Result<PathBuf> {
    let Some((root, _, _)) = managed(executable) else {
        return Ok(executable.to_owned());
    };
    let binary = executable
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("invalid managed executable name"))?;
    // QueryFullProcessImageNameW returns a DOS path, while activation targets
    // are canonical verbatim paths. Compare filesystem-resolved spellings;
    // lexical equality would reject the currently running installed binary.
    let root = root.canonicalize()?;
    let executable = executable.canonicalize()?;
    if target(&root, binary)?.as_deref() != Some(executable.as_path()) {
        return Err(invalid(
            "this copy is no longer active; reopen before configuring integrations",
        ));
    }
    let launcher = root.join("bin").join(binary);
    if launcher_root(&launcher)?.as_deref() != Some(root.as_path()) {
        return Err(invalid("managed Windows launcher is missing"));
    }
    Ok(launcher)
}

/// A copied bootstrap adds one process around an installed command. Unwrap
/// only that exact, live, receipt-verified image in the same store. PID births
/// refuse a reused bootstrap or caller PID; argv and environment supply no
/// identity evidence. Otherwise callers retain the actual OS parent.
#[cfg(windows)]
pub(crate) fn original_parent(actual_parent: u32) -> Option<u32> {
    let executable = crate::procinfo::executable_path().ok()?;
    let (root, _, _) = managed(&executable)?;
    let child_birth = crate::procinfo::start_time(std::process::id())?;
    let parent = crate::procinfo::inspect(actual_parent)?;
    if parent.ppid <= 1 || parent.ppid == actual_parent || parent.ppid == std::process::id() {
        return None;
    }
    let parent_birth = crate::procinfo::start_time(actual_parent)?;
    let caller_birth = crate::procinfo::start_time(parent.ppid)?;
    if parent_birth > child_birth || caller_birth > parent_birth {
        return None;
    }
    let parent_image = crate::procinfo::executable_path_of(actual_parent)
        .ok()?
        .canonicalize()
        .ok()?;
    if parent_image.file_name() != executable.file_name()
        || launcher_root(&parent_image).ok()?? != root.canonicalize().ok()?
        || crate::procinfo::start_time(actual_parent) != Some(parent_birth)
        || crate::procinfo::start_time(parent.ppid) != Some(caller_birth)
    {
        return None;
    }
    Some(parent.ppid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_record(path: &Path, value: &Value) {
        let mut file = dirs::create_private_file(path).unwrap();
        serde_json::to_writer(&mut file, value).unwrap();
        file.flush().unwrap();
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp
            .path()
            .canonicalize()
            .unwrap()
            .join("trial space-λ")
            .join(STORE_SUFFIX);
        dirs::secure_state_dir(&root).unwrap();
        let target = root
            .join("versions")
            .join("a".repeat(64))
            .join("AgentDocker/agentdocker.exe");
        let launcher = root.join("bin/agentdocker.exe");
        for path in [&target, &launcher] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"test executable").unwrap();
        }
        write_record(
            &root.join("activation.json"),
            &serde_json::json!({"format":1,
            "current":{"id":"a".repeat(64),"payload":"AgentDocker","installation_lock":1}}),
        );
        write_record(
            &root.join("launcher.json"),
            &serde_json::json!({"format":1,
            "binary_sha256":{"agentdocker.exe":hash(&launcher).unwrap()}}),
        );
        (temp, root, target, launcher)
    }

    #[test]
    fn a_recorded_windows_launcher_selects_only_its_exact_pinned_payload() {
        let (_temp, root, selected, launcher) = fixture();
        assert_eq!(launcher_target(&launcher).unwrap(), Some(selected.clone()));
        assert_eq!(
            target(&root, "agentdocker.exe").unwrap(),
            Some(selected.clone())
        );
        let (observed_root, version, id) = managed(&selected).unwrap();
        assert_eq!(observed_root, root);
        assert_eq!(version.join("AgentDocker/agentdocker.exe"), selected);
        assert_eq!(id, "a".repeat(64));
        assert!(launcher_target(&selected).unwrap().is_none());
        assert!(target(&root, "../agentdocker.exe").is_err());
    }

    #[test]
    fn windows_registration_requires_the_active_release_and_verified_launcher() {
        let (_temp, root, selected, launcher) = fixture();
        assert_eq!(stable_executable(&selected).unwrap(), launcher);
        let obsolete = root
            .join("versions")
            .join("b".repeat(64))
            .join("AgentDocker/agentdocker.exe");
        std::fs::create_dir_all(obsolete.parent().unwrap()).unwrap();
        std::fs::write(&obsolete, b"obsolete executable").unwrap();
        assert!(
            stable_executable(&obsolete)
                .unwrap_err()
                .to_string()
                .contains("no longer active")
        );
        std::fs::write(&launcher, b"changed executable").unwrap();
        assert!(
            stable_executable(&selected)
                .unwrap_err()
                .to_string()
                .contains("modified")
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_registration_accepts_the_dos_spelling_of_the_active_image() {
        let (_temp, _root, selected, launcher) = fixture();
        let selected = selected.canonicalize().unwrap();
        let dos = Path::new(selected.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
        assert_ne!(dos, selected);
        assert_eq!(
            stable_executable(dos).unwrap(),
            launcher.canonicalize().unwrap()
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "owned subprocess invoked by copied_windows_bootstrap_starts_and_pins_its_payload"]
    fn native_bootstrap_child() {
        super::super::redirect_managed_launcher().unwrap();
        let executable = crate::procinfo::executable_path().unwrap();
        let (root, _, id) = managed(&executable).expect("selected immutable executable");
        assert_eq!(
            stable_executable(&executable).unwrap(),
            root.canonicalize().unwrap().join("bin/agentdocker.exe"),
            "the kernel-reported running image can register its stable launcher"
        );
        let _pin = super::super::pin_current_executable()
            .unwrap()
            .expect("lifetime pin");
        assert!(
            crate::lock::try_exclusive(&super::super::pin_path(&root, id).unwrap())
                .unwrap()
                .is_none()
        );
        println!("WINDOWS_BOOTSTRAP_SELECTED_PINNED_PAYLOAD");
        let expected: u32 = std::env::var("AGENTDOCKER_BOOTSTRAP_FIXTURE_PARENT")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            crate::procinfo::parent_id(),
            expected,
            "bootstrap preserves the original MCP host parent"
        );
        // The parent bootstrap still waits for this immutable child. Exercise
        // the loaded image, not merely a regular file held open for reading.
        let launcher = root.join("bin/agentdocker.exe");
        let retired = root.join("retired-test");
        dirs::secure_state_dir(&retired).unwrap();
        files::retire_open_regular(&launcher, &retired.join("agentdocker.exe")).unwrap();
        assert!(
            !launcher.exists(),
            "loaded bootstrap leaves the public namespace"
        );
    }

    #[cfg(windows)]
    #[test]
    fn copied_windows_bootstrap_starts_and_pins_its_payload() {
        let (temp, root, selected, launcher) = fixture();
        let executable = crate::procinfo::executable_path().unwrap();
        for path in [&selected, &launcher] {
            std::fs::copy(&executable, path).unwrap();
        }
        std::fs::remove_file(root.join("launcher.json")).unwrap();
        write_record(
            &root.join("launcher.json"),
            &serde_json::json!({"format":1,
            "binary_sha256":{"agentdocker.exe":hash(&launcher).unwrap()}}),
        );
        let expected_parent = std::process::id().to_string();
        let result = crate::command::run_with_env(
            temp.path(),
            &[
                launcher.to_str().unwrap().into(),
                "--exact".into(),
                "installation::windows::tests::native_bootstrap_child".into(),
                "--ignored".into(),
                "--nocapture".into(),
            ],
            std::time::Duration::from_secs(20),
            &[(
                "AGENTDOCKER_BOOTSTRAP_FIXTURE_PARENT",
                Some(std::ffi::OsStr::new(&expected_parent)),
            )],
        )
        .unwrap();
        assert!(result.success, "{}", result.text);
        assert!(
            result
                .text
                .contains("WINDOWS_BOOTSTRAP_SELECTED_PINNED_PAYLOAD")
        );
        // The bootstrap has now exited; its retired image can be removed normally.
        std::fs::remove_file(root.join("retired-test/agentdocker.exe")).unwrap();
    }

    #[test]
    fn modified_and_inactive_windows_launchers_refuse_to_run() {
        let (_temp, root, _selected, launcher) = fixture();
        std::fs::remove_file(root.join("activation.json")).unwrap();
        assert!(target(&root, "agentdocker.exe").unwrap().is_none());
        assert!(launcher_target(&launcher).is_err());
        std::fs::write(&launcher, b"changed executable").unwrap();
        assert!(
            launcher_target(&launcher)
                .unwrap_err()
                .to_string()
                .contains("modified")
        );
    }

    #[test]
    fn windows_activation_readers_observe_complete_atomic_records() {
        let (_temp, root, first, _launcher) = fixture();
        let second = root
            .join("versions")
            .join("b".repeat(64))
            .join("AgentDocker/agentdocker.exe");
        std::fs::create_dir_all(second.parent().unwrap()).unwrap();
        std::fs::write(&second, b"second executable").unwrap();
        let barrier = std::sync::Barrier::new(5);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    barrier.wait();
                    for _ in 0..200 {
                        let observed = target(&root, "agentdocker.exe").unwrap().unwrap();
                        assert!(observed == first || observed == second);
                    }
                });
            }
            barrier.wait();
            for index in 0..100 {
                let staged = root.join("activation.staged");
                let id = if index % 2 == 0 { "b" } else { "a" }.repeat(64);
                write_record(
                    &staged,
                    &serde_json::json!({"format":1,
                    "current":{"id":id,"payload":"AgentDocker","installation_lock":1}}),
                );
                files::publish_snapshot(&staged, &root.join("activation.json")).unwrap();
            }
        });
        assert_eq!(target(&root, "agentdocker.exe").unwrap(), Some(first));
    }

    #[test]
    fn damaged_windows_activation_never_falls_back_to_a_portable_launch() {
        let (_temp, root, _selected, launcher) = fixture();
        let path = root.join("activation.json");
        for value in [
            serde_json::json!({"format":2}),
            serde_json::json!({"format":1,"current":{"id":"../outside","payload":"AgentDocker","installation_lock":1}}),
            serde_json::json!({"format":1,"current":{"id":"a".repeat(64),"payload":"../outside","installation_lock":1}}),
            serde_json::json!({"format":1,"current":{"id":"a".repeat(64),"payload":"AgentDocker","installation_lock":0}}),
        ] {
            std::fs::remove_file(&path).unwrap();
            write_record(&path, &value);
            assert!(launcher_target(&launcher).is_err());
        }
        std::fs::write(&path, vec![b' '; 65537]).unwrap();
        assert!(
            launcher_target(&launcher)
                .unwrap_err()
                .to_string()
                .contains("bound")
        );
    }

    #[test]
    fn atomic_windows_record_reads_still_refuse_hard_links() {
        let (_temp, root, _selected, launcher) = fixture();
        let alias = root.join("foreign-record-alias.json");
        std::fs::hard_link(root.join("activation.json"), &alias).unwrap();
        assert!(launcher_target(&launcher).is_err());
        std::fs::remove_file(alias).unwrap();
        assert!(launcher_target(&launcher).unwrap().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn atomic_windows_record_reads_preserve_permissions_and_refuse_final_links() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let (_temp, root, _selected, launcher) = fixture();
        let record = root.join("activation.json");
        std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(launcher_target(&launcher).is_err());
        assert_eq!(
            std::fs::metadata(&record).unwrap().permissions().mode() & 0o777,
            0o666
        );
        std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600)).unwrap();
        let outside = root.join("other-record.json");
        std::fs::rename(&record, &outside).unwrap();
        symlink(&outside, &record).unwrap();
        assert!(launcher_target(&launcher).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn redirected_windows_payloads_are_refused_even_when_the_file_exists() {
        let (_temp, root, selected, launcher) = fixture();
        let payload = selected.parent().unwrap();
        let outside = root.join("other-payload");
        std::fs::rename(payload, &outside).unwrap();
        std::os::unix::fs::symlink(&outside, payload).unwrap();
        assert!(
            launcher_target(&launcher)
                .unwrap_err()
                .to_string()
                .contains("escaped")
        );
    }
}
