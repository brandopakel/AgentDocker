//! Lifetime protection for binaries in a managed desktop installation.
//!
//! Pin files are stable outside version directories and are never deleted by
//! retention. An exclusive maintenance lock and shared running binaries use
//! the same inode; a startup losing that race exits before using the payload.
use crate::{dirs, lock};
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(any(windows, test))]
pub mod windows;

pub const LOCK_FORMAT: u32 = 1;
// Windows uses a receipt-checked bootstrap process rather than Unix exec.
// Earlier Windows packages advertised the Mac-only format 1 and must not be
// mistaken for bootstraps that understand the native activation record.
pub const LAUNCHER_REDIRECT_FORMAT: u32 = if cfg!(windows) { 2 } else { 1 };

/// A visible macOS application is an intact signed copy. Its entrypoints run
/// the selected immutable release before parsing commands or starting services.
/// Ownership is outside the signed bundle, in the installer's existing record.
pub fn redirect_managed_launcher() -> io::Result<()> {
    #[cfg(windows)]
    {
        let executable = crate::procinfo::executable_path()?;
        if let Some(target) = windows::launcher_target(&executable)? {
            let pin = pin_executable(&target)?.ok_or_else(|| {
                io::Error::other("Windows launcher target is not an immutable installed release")
            })?;
            // Inherit the terminal, pipes, environment and argument boundaries.
            // Do not put this in a kill-on-close job: a short-lived CLI can
            // legitimately start a daemon that must survive the CLI's exit.
            let status = std::process::Command::new(target)
                .args(std::env::args_os().skip(1))
                .status()?;
            drop(pin);
            std::process::exit(status.code().unwrap_or(1));
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::process::CommandExt;
        let executable = crate::procinfo::executable_path()?;
        if let Some(target) = launcher_target(&executable, std::env::home_dir().as_deref())? {
            let _pin = pin_executable(&target)?.ok_or_else(|| {
                io::Error::other("launcher target is not an immutable installed release")
            })?;
            // exec preserves the invocation's terminal, arguments and process
            // identity. CLOEXEC closes this pin when the new image loads; that
            // image acquires its own pin before using release resources. If
            // activation and pruning win this handoff, startup fails closed
            // under pin_executable's lock and existence check.
            return Err(std::process::Command::new(target)
                .args(std::env::args_os().skip(1))
                .exec());
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", all(test, unix)))]
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

/// Serialize service registration with maintenance of the selected stores.
///
/// A daemon selected from PATH can belong to a different release or store than
/// the calling CLI. Protect both until the definition and manager registration
/// are committed. The permanent service lock is acquired before executable
/// pins; maintenance holds the same lock from inventory through deletion.
/// Portable executables have no managed store and create no installation state.
pub fn guard_service_registration(executables: &[PathBuf]) -> io::Result<Vec<lock::Lock>> {
    guard_service_references(executables, &[])
}

/// Protect every managed store a service will use before publishing it.
///
/// Executables must exist; data paths may name a future log or state directory.
/// Resolve aliases first, acquire store guards in path order, then pin every
/// referenced immutable version. Hold these guards through manager registration.
/// This also protects tunnel binaries and data in a different installation from
/// the controller; ordinary paths outside managed stores create no state.
pub fn guard_service_references(
    executables: &[PathBuf],
    data_paths: &[PathBuf],
) -> io::Result<Vec<lock::Lock>> {
    let executables = executables
        .iter()
        .map(|path| path.canonicalize())
        .collect::<io::Result<Vec<_>>>()?;
    let data_paths = data_paths
        .iter()
        .map(|path| crate::project::try_canonical(path))
        .collect::<io::Result<Vec<_>>>()?;
    let references = executables
        .iter()
        .chain(&data_paths)
        .map(|path| Ok((path, managed_store(path)?)))
        .collect::<io::Result<Vec<_>>>()?;
    let roots: BTreeSet<_> = references
        .iter()
        .filter_map(|(_, root)| root.clone())
        .collect();
    let versions: BTreeSet<_> = references
        .iter()
        .filter_map(|(path, root)| {
            let root = root.as_ref()?;
            let versions = root.join("versions");
            let id = path
                .strip_prefix(&versions)
                .ok()?
                .components()
                .next()?
                .as_os_str()
                .to_str()?;
            // Unrecognized names are never deletion candidates; protect their
            // store without pretending they are immutable release identities.
            if pin_path(root, id).is_err() {
                return None;
            }
            Some((root.clone(), versions.join(id), id.to_owned()))
        })
        .collect();
    let mut guards = Vec::new();
    for root in roots {
        guards.push(
            service_inventory_guard(&root, true)?
                .ok_or_else(|| io::Error::other("service registration guard was not created"))?,
        );
    }
    for (root, version, id) in versions {
        guards.push(pin_version(&root, &version, &id)?);
    }
    if executables.iter().any(|path| !path.is_file()) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "service executable disappeared before registration",
        ));
    }
    Ok(guards)
}

// Stable bootstrap paths and store-level data still need the service guard
// even when they name no immutable version. A bare similarly named directory
// with no installation inventory is not a managed store.
fn managed_store(path: &Path) -> io::Result<Option<PathBuf>> {
    let suffix = if cfg!(windows) {
        "AgentDocker/desktop"
    } else {
        ".local/share/agentdocker/desktop"
    };
    for root in path.ancestors().filter(|root| root.ends_with(suffix)) {
        match root.join("versions").symlink_metadata() {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                return Ok(Some(root.to_owned()));
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "managed version inventory is not a regular directory",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

/// Hold the service inventory stable through a maintenance operation.
/// A preview opens only an existing lock and creates no files. Actual
/// registration/deletion creates the owner-only permanent lock if needed.
pub fn service_inventory_guard(root: &Path, create: bool) -> io::Result<Option<lock::Lock>> {
    let path = root.join("services.lock");
    if create {
        dirs::secure_state_dir(root)?;
        dirs::private_file(&path, true, false)?;
    } else {
        match path.symlink_metadata() {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    lock::try_exclusive_existing(&path)?
        .map(Some)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "service registration or desktop maintenance is in progress; try again when it finishes",
            )
        })
}

/// Candidate-version pins held before reading service registrations.
///
/// A legacy registrar may hold only its executable's lifetime pin, without
/// taking services.lock. Remember that version as busy even if the registrar
/// publishes a service and exits during the subsequent inventory query.
#[derive(Default)]
pub struct VersionInventoryGuard {
    busy: BTreeSet<String>,
    _pins: Vec<lock::Lock>,
}

impl VersionInventoryGuard {
    pub fn is_busy(&self, version: &str) -> bool {
        self.busy.contains(version)
    }
}

/// Reserve inactive candidates before inspecting persisted/loaded services.
/// Hold the result through deletion. Preview opens existing permanent pins
/// only; apply creates missing pins and must recompute its service inventory.
pub fn reserve_versions_for_inventory(
    root: &Path,
    versions: &[String],
    applying: bool,
) -> io::Result<VersionInventoryGuard> {
    let mut result = VersionInventoryGuard::default();
    if applying && !versions.is_empty() {
        dirs::secure_state_dir(&root.join("pins"))?;
    }
    for version in versions {
        let path = pin_path(root, version)?;
        if applying {
            dirs::private_file(&path, true, false)?;
        } else {
            match path.symlink_metadata() {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            }
        }
        match lock::try_exclusive_existing(&path)? {
            Some(pin) => result._pins.push(pin),
            None => {
                result.busy.insert(version.clone());
            }
        }
    }
    Ok(result)
}

/// Locate only our versioned layout. An ordinary checkout/package is unpinned.
#[cfg(not(windows))]
fn managed(executable: &Path) -> Option<(PathBuf, PathBuf, &str)> {
    let versions = executable
        .ancestors()
        .find(|path| path.ends_with(".local/share/agentdocker/desktop/versions"))?;
    let relative = executable.strip_prefix(versions).ok()?;
    let id = relative.components().next()?.as_os_str().to_str()?;
    Some((versions.parent()?.to_owned(), versions.join(id), id))
}

#[cfg(windows)]
fn managed(executable: &Path) -> Option<(PathBuf, PathBuf, &str)> {
    windows::managed(executable)
}

/// The `agentd` of the release the managed installation currently
/// activates, when `executable` belongs to a managed installation and
/// that release differs from the executable's own. This is what a daemon
/// reloads to after `desktop install`, `update` or `rollback`: the
/// daemon runs from a pinned version directory, so its own path always
/// names the release it started from, never the one activated since.
#[cfg(not(windows))]
pub fn activated_daemon(executable: &Path) -> Option<PathBuf> {
    let (root, version, _) = managed(executable)?;
    let payload = root.join("current").join("payload");
    // Where this platform's payload keeps its binaries: an application
    // bundle on macOS, a plain tree elsewhere. Never the other one, even
    // if a payload carries both.
    let inside = if cfg!(target_os = "macos") {
        "Contents/MacOS/agentd"
    } else {
        "bin/agentd"
    };
    let candidate = payload.join(inside);
    if !candidate.is_file() {
        return None;
    }
    let resolved = std::fs::canonicalize(&candidate).ok()?;
    // Under the same versions directory, and not the release already
    // running: an activation is only a reload target when it is new.
    // Everything is compared resolved, since a home may itself sit behind
    // a symlink.
    let versions = std::fs::canonicalize(root.join("versions")).ok()?;
    let running = std::fs::canonicalize(&version).ok()?;
    (resolved.starts_with(&versions) && !resolved.starts_with(&running)).then_some(resolved)
}

#[cfg(windows)]
pub fn activated_daemon(executable: &Path) -> Option<PathBuf> {
    let (root, version, _) = managed(executable)?;
    let target = windows::target(&root, "agentd.exe").ok()??;
    let running = version.canonicalize().ok()?;
    (!target.starts_with(running)).then_some(target)
}

/// The `agentd` a client starts when no daemon answers. A client from a
/// managed release that is no longer the activated one starts the activated
/// release's daemon: an app left open across `desktop install` must not
/// bring its own older daemon back once the old one stops. Otherwise the
/// `agentd` beside the client, so a build in `target/` starts the matching
/// daemon, else `agentd` on `PATH`.
pub fn daemon_to_start(client: Option<&Path>) -> PathBuf {
    let name = format!("agentd{}", std::env::consts::EXE_SUFFIX);
    client
        .and_then(|me| {
            activated_daemon(me).or_else(|| {
                me.parent()
                    .map(|dir| dir.join(&name))
                    .filter(|sibling| sibling.is_file())
            })
        })
        .unwrap_or_else(|| PathBuf::from(name))
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
    let pin = pin_version(&root, &version, id)?;
    if !executable.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "desktop release was removed; reopen the active installation",
        ));
    }
    Ok(Some(pin))
}

fn pin_version(root: &Path, version: &Path, id: &str) -> io::Result<lock::Lock> {
    let path = pin_path(root, id)?;
    dirs::secure_state_dir(root)?;
    dirs::secure_state_dir(&root.join("pins"))?;
    let pin = lock::try_shared(&path)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "desktop release is being removed; reopen the active installation",
        )
    })?;
    // A pruner may have won before this lock was opened. Pin files outlive
    // deletion, so all referenced versions must still exist under the pin.
    if !version.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "desktop release was removed; reopen the active installation",
        ));
    }
    Ok(pin)
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

    /// A daemon running from release A reloads to the release the
    /// installation activates, B, and to nothing when A is still the one
    /// activated, or when the executable is not managed at all.
    #[cfg(unix)]
    #[test]
    fn the_activated_daemon_is_the_other_release_under_current() {
        use std::os::unix::fs::symlink;
        let (temp, root, executable) = fixture();
        let running = executable.with_file_name("agentd");
        std::fs::write(&running, "release a").unwrap();
        let versions = root.join("versions");
        let b = versions.join("b".repeat(64)).join("payload");
        std::fs::create_dir_all(b.join("bin")).unwrap();
        std::fs::create_dir_all(b.join("Contents/MacOS")).unwrap();
        std::fs::write(b.join("bin/agentd"), "release b").unwrap();
        std::fs::write(b.join("Contents/MacOS/agentd"), "release b").unwrap();

        // Nothing activated yet.
        assert_eq!(activated_daemon(&running), None);
        // A activated: the running release, so nothing to reload to.
        let generations = root.join("generations");
        std::fs::create_dir_all(&generations).unwrap();
        let gen_a = generations.join("1");
        std::fs::create_dir_all(&gen_a).unwrap();
        symlink(running.parent().unwrap(), gen_a.join("payload")).unwrap();
        symlink(&gen_a, root.join("current")).unwrap();
        // The fixture's release A keeps its binaries beside `payload`, so
        // put the daemon where the layout would.
        std::fs::create_dir_all(running.parent().unwrap().join("bin")).unwrap();
        std::fs::write(running.parent().unwrap().join("bin/agentd"), "release a").unwrap();
        assert_eq!(
            activated_daemon(&running),
            None,
            "already the active release"
        );
        // A client of the active release starts the daemon beside it.
        let client = executable.with_file_name("agentdocker-ui");
        std::fs::write(&client, "release a").unwrap();
        assert_eq!(daemon_to_start(Some(&client)), running);
        // B activated: that is the reload target, resolved to its real path.
        let gen_b = generations.join("2");
        std::fs::create_dir_all(&gen_b).unwrap();
        symlink(&b, gen_b.join("payload")).unwrap();
        std::fs::remove_file(root.join("current")).unwrap();
        symlink(&gen_b, root.join("current")).unwrap();
        let expected = if cfg!(target_os = "macos") {
            b.join("Contents/MacOS/agentd")
        } else {
            b.join("bin/agentd")
        };
        assert_eq!(
            activated_daemon(&running),
            Some(std::fs::canonicalize(&expected).unwrap())
        );
        // A client still open from release A starts B's daemon, not its own.
        assert_eq!(
            daemon_to_start(Some(&client)),
            std::fs::canonicalize(&expected).unwrap()
        );
        // An unmanaged daemon has no activated release to speak of.
        let elsewhere = temp.path().join("agentd");
        std::fs::write(&elsewhere, "checkout build").unwrap();
        assert_eq!(activated_daemon(&elsewhere), None);
        // An unmanaged client starts its sibling, and with none, `agentd`
        // on PATH.
        let checkout_client = temp.path().join("agentdocker");
        assert_eq!(daemon_to_start(Some(&checkout_client)), elsewhere);
        let lone = temp.path().join("lone").join("agentdocker");
        let bare = PathBuf::from(format!("agentd{}", std::env::consts::EXE_SUFFIX));
        assert_eq!(daemon_to_start(Some(&lone)), bare);
        assert_eq!(daemon_to_start(None), bare);
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
        let root = temp.path().join(if cfg!(windows) {
            "AgentDocker/desktop"
        } else {
            ".local/share/agentdocker/desktop"
        });
        // Match installation's private-state creation. On an elevated Windows
        // runner, ordinary create_dir_all can assign Administrators ownership,
        // which is intentionally refused for the final application state.
        dirs::secure_state_dir(&root).unwrap();
        let executable = root
            .join("versions")
            .join("a".repeat(64))
            .join(if cfg!(windows) {
                "AgentDocker/agentdocker.exe"
            } else {
                "payload/agentdocker"
            });
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
        let _maintenance = released_lock(&path, lock::try_exclusive);
    }

    #[test]
    fn service_registration_protects_every_selected_store_before_publication() {
        let (_first, first_root, first_exe) = fixture();
        let (_second, second_root, second_exe) = fixture();
        let selected = [first_exe, second_exe];
        let registration = guard_service_registration(&selected).unwrap();
        for root in [&first_root, &second_root] {
            assert_eq!(
                service_inventory_guard(root, true).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
            assert!(
                lock::try_exclusive_existing(&pin_path(root, &"a".repeat(64)).unwrap())
                    .unwrap()
                    .is_none(),
                "the separately selected daemon must be pinned too"
            );
        }
        drop(registration);
        let _first_inventory = released_lock(
            &first_root.join("services.lock"),
            lock::try_exclusive_existing,
        );
        let _second_inventory = released_lock(
            &second_root.join("services.lock"),
            lock::try_exclusive_existing,
        );
        assert_eq!(
            guard_service_registration(&selected).unwrap_err().kind(),
            io::ErrorKind::WouldBlock,
            "registration must refuse before publishing across a guarded inventory"
        );
    }

    #[test]
    fn service_data_and_tunnel_references_pin_other_stores_until_registration_finishes() {
        let (_controller_temp, controller_root, controller) = fixture();
        let (_data_temp, data_root, sample) = fixture();
        let payload = sample.parent().unwrap();
        let tunnel = payload.join("private-tunnel.exe");
        let feed = payload.join("egress.json");
        let project = payload.join("project");
        std::fs::write(&tunnel, "fixture tunnel").unwrap();
        std::fs::write(&feed, "fixture feed").unwrap();
        std::fs::create_dir(&project).unwrap();
        let future_log = payload.join("future/log/output.log");
        let registration =
            guard_service_references(&[controller, tunnel], &[feed, project, future_log.clone()])
                .unwrap();
        assert!(
            !future_log.exists(),
            "reference protection must not create data files"
        );
        for root in [&controller_root, &data_root] {
            assert_eq!(
                service_inventory_guard(root, true).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
            assert!(
                lock::try_exclusive_existing(&pin_path(root, &"a".repeat(64)).unwrap())
                    .unwrap()
                    .is_none()
            );
        }
        drop(registration);
        let _maintenance = released_lock(
            &data_root.join("services.lock"),
            lock::try_exclusive_existing,
        );
        assert_eq!(
            guard_service_references(&[], &[future_log])
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn stable_service_bootstrap_and_store_data_guard_deactivation_without_data_creation() {
        for use_bootstrap in [true, false] {
            let (_temp, root, _sample) = fixture();
            let bootstrap = root.join("bin/agentdocker.exe");
            std::fs::create_dir(bootstrap.parent().unwrap()).unwrap();
            std::fs::write(&bootstrap, "fixture stable bootstrap").unwrap();
            let future_state = root.join("service-state/state.json");
            let (executables, data) = if use_bootstrap {
                (vec![bootstrap], vec![])
            } else {
                (vec![], vec![future_state.clone()])
            };
            let _guard = guard_service_references(&executables, &data).unwrap();
            assert_eq!(
                service_inventory_guard(&root, true).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
            assert!(!future_state.exists());
        }
    }

    #[test]
    fn unknown_version_names_keep_the_store_guard_and_malformed_inventory_refuses() {
        let (_temp, root, _sample) = fixture();
        let unrecognized = root.join("versions/user-notes/project");
        let guard = guard_service_references(&[], &[unrecognized]).unwrap();
        assert_eq!(
            service_inventory_guard(&root, true).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(guard);
        std::fs::rename(root.join("versions"), root.join("retained-fixture")).unwrap();
        std::fs::write(root.join("versions"), "malformed inventory").unwrap();
        assert_eq!(
            guard_service_references(&[], &[root.join("future-state")])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn future_data_in_an_absent_installation_does_not_create_a_store() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join(if cfg!(windows) {
            "AgentDocker/desktop"
        } else {
            ".local/share/agentdocker/desktop"
        });
        let future = root
            .join("versions")
            .join("a".repeat(64))
            .join("state/file");
        assert!(guard_service_references(&[], &[future]).unwrap().is_empty());
        assert!(!root.exists());
    }

    #[cfg(unix)]
    #[test]
    fn service_reference_aliases_resolve_to_the_store_even_with_a_missing_suffix() {
        let (temp, root, sample) = fixture();
        let alias = temp.path().join("resource-alias");
        std::os::unix::fs::symlink(sample.parent().unwrap(), &alias).unwrap();
        let guard = guard_service_references(&[], &[alias.join("future/output.log")]).unwrap();
        assert!(
            lock::try_exclusive_existing(&pin_path(&root, &"a".repeat(64)).unwrap())
                .unwrap()
                .is_none()
        );
        drop(guard);
        let _released = released_lock(&root.join("services.lock"), lock::try_exclusive_existing);
    }

    #[test]
    fn service_preview_and_portable_registration_do_not_create_managed_state() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("absent-installation");
        assert!(service_inventory_guard(&root, false).unwrap().is_none());
        assert!(!root.exists());
        let portable = temp.path().join("agentd");
        std::fs::write(&portable, "fixture").unwrap();
        assert!(guard_service_registration(&[portable]).unwrap().is_empty());
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    // Libtest runs process-spawning tests in this same process. A concurrent
    // fork inherits an open lock until exec closes its CLOEXEC descriptor.
    // Require the real lock to release within a bound before testing the next
    // state; never accept contention as evidence that a deleted file is safe.
    fn released_lock(
        path: &Path,
        acquire: fn(&Path) -> io::Result<Option<lock::Lock>>,
    ) -> lock::Lock {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(held) = acquire(path).unwrap() {
                return held;
            }
            assert!(std::time::Instant::now() < deadline, "pin never released");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn maintenance_blocks_startup_and_deleted_versions_cannot_rejoin() {
        let (_temp, root, exe) = fixture();
        drop(pin_executable(&exe).unwrap());
        let path = pin_path(&root, &"a".repeat(64)).unwrap();
        let held = released_lock(&path, lock::try_exclusive);
        assert_eq!(
            pin_executable(&exe).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        std::fs::remove_file(&exe).unwrap();
        drop(held);
        let _shared = released_lock(&path, lock::try_shared);
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
