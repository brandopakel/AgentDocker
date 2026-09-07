//! Directories the daemon and its clients must agree on and be able to
//! trust: a socket directory under a shared temp root is safe to bind in
//! only if it is ours alone.

use std::io;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use agentdocker_core::paths;

#[cfg(windows)]
#[path = "dirs/windows.rs"]
mod windows;
#[cfg(windows)]
pub use windows::{check_socket_parent, ensure_private_dir, private_file, secure_state_dir};
#[cfg(windows)]
pub(crate) use windows::{current_sid, process_sid};

/// Protect app-owned state, including existing 0755 installations. Validate
/// the final component without following symlinks, then chmod the opened
/// directory rather than a second path lookup. Never recurse into its contents.
#[cfg(unix)]
pub fn secure_state_dir(dir: &Path) -> io::Result<()> {
    ensure_private_dir(dir)?;
    let handle = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(dir)?;
    validate_owner(&handle.metadata()?, dir)?;
    handle.set_permissions(std::fs::Permissions::from_mode(0o700))
}

/// Open an app-owned private regular file without truncation. Existing 0644
/// data is narrowed to 0600; symlinks, hard links and foreign/writable files
/// are refused before changing their contents or permissions.
#[cfg(unix)]
pub fn private_file(path: &Path, create: bool, append: bool) -> io::Result<std::fs::File> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => validate_file(&meta, path)?,
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error),
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .append(append)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    validate_file(&file.metadata()?, path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

#[cfg(unix)]
fn validate_owner(meta: &std::fs::Metadata, path: &Path) -> io::Result<()> {
    // SAFETY: geteuid has no preconditions.
    let me = unsafe { libc::geteuid() };
    if meta.uid() != me || meta.mode() & 0o022 != 0 {
        return Err(io::Error::other(format!(
            "{} must be owned by this user and not writable by others",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn validate_file(meta: &std::fs::Metadata, path: &Path) -> io::Result<()> {
    if !meta.is_file() || meta.nlink() != 1 {
        return Err(io::Error::other(format!(
            "{} must be a regular file with one link",
            path.display()
        )));
    }
    validate_owner(meta, path)
}

/// Create `dir` for this user alone (mode `0700`) when it is missing, and
/// refuse it when it is not a directory we own that nobody else can write
/// to. A pre-planted directory or symlink under `/tmp` fails here instead
/// of receiving our sockets.
#[cfg(unix)]
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if !meta.is_dir() => {
            return Err(io::Error::other(format!(
                "{} exists and is not a directory",
                dir.display()
            )));
        }
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        Err(err) => return Err(err),
    }
    let meta = std::fs::symlink_metadata(dir)?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    if meta.uid() != me {
        return Err(io::Error::other(format!(
            "{} is owned by uid {}, not by this user ({me})",
            dir.display(),
            meta.uid()
        )));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(io::Error::other(format!(
            "{} is writable by others (mode {:o}); make it 0700",
            dir.display(),
            meta.mode() & 0o777
        )));
    }
    Ok(())
}

/// Validate managed fallback directories before trusting an existing socket.
/// Missing directories are left for autostart to create; explicit sockets outside
/// the managed /tmp namespace retain their existing caller-selected semantics.
#[cfg(unix)]
pub fn check_socket_parent(socket: &Path) -> io::Result<()> {
    let Some(parent) = socket.parent() else {
        return Ok(());
    };
    let managed_name = parent
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| {
            n.strip_prefix("agentdocker-")
                .is_some_and(|hash| hash.len() == 12 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
        });
    let tmp = Path::new("/tmp");
    let canonical_tmp = tmp.canonicalize().unwrap_or_else(|_| tmp.into());
    if managed_name
        && (parent.parent() == Some(tmp) || parent.parent() == Some(canonical_tmp.as_path()))
    {
        match std::fs::symlink_metadata(parent) {
            Ok(_) => ensure_private_dir(parent)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// The daemon's home as every process should spell it: the configured
/// path, canonical when it exists, so a symlinked `AGENTDOCKER_HOME` names
/// the same socket directory from the daemon, every client, and an
/// installed service alike.
pub fn home() -> PathBuf {
    canonical_home(paths::default_home())
}

pub fn canonical_home(home: PathBuf) -> PathBuf {
    crate::project::canonical(&home)
}

/// The directory a home's sockets live in, existing and safe: the home
/// itself is simply created, a fallback under the runtime directory must
/// be private. Both binaries call this before binding or locking there.
pub fn socket_dir_ready(home: &Path) -> io::Result<PathBuf> {
    let dir = paths::socket_dir(home);
    if cfg!(windows) {
        secure_state_dir(&dir)?;
    } else if dir == home {
        std::fs::create_dir_all(&dir)?;
    } else {
        ensure_private_dir(&dir)?;
    }
    Ok(dir)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn private_state_creation_and_legacy_permissions_preserve_contents() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        secure_state_dir(&state).unwrap();
        let data = state.join("state.db");
        private_file(&data, true, false)
            .unwrap()
            .write_all(b"retained")
            .unwrap();
        assert_eq!(std::fs::metadata(&state).unwrap().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(&data).unwrap().mode() & 0o777, 0o600);
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o644)).unwrap();
        secure_state_dir(&state).unwrap();
        private_file(&data, false, true)
            .unwrap()
            .write_all(b"-appended")
            .unwrap();
        assert_eq!(std::fs::read(&data).unwrap(), b"retained-appended");
        assert_eq!(std::fs::metadata(&state).unwrap().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(&data).unwrap().mode() & 0o777, 0o600);
    }

    #[test]
    fn private_state_refuses_links_without_changing_their_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("unrelated");
        std::fs::write(&target, b"untouched").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        let symlink = tmp.path().join("symlink");
        std::os::unix::fs::symlink(&target, &symlink).unwrap();
        assert!(private_file(&symlink, true, false).is_err());
        let hardlink = tmp.path().join("hardlink");
        std::fs::hard_link(&target, &hardlink).unwrap();
        assert!(private_file(&hardlink, true, false).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
        assert_eq!(std::fs::metadata(&target).unwrap().mode() & 0o777, 0o644);
        let dir_link = tmp.path().join("directory-link");
        std::os::unix::fs::symlink(tmp.path(), &dir_link).unwrap();
        assert!(secure_state_dir(&dir_link).is_err());
    }

    #[test]
    fn private_dir_is_created_0700_and_others_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let fresh = tmp.path().join("sockets");
        ensure_private_dir(&fresh).unwrap();
        let mode = std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        ensure_private_dir(&fresh).unwrap();

        let open = tmp.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
        let err = ensure_private_dir(&open).unwrap_err().to_string();
        assert!(err.contains("writable by others"), "{err}");

        let file = tmp.path().join("file");
        std::fs::write(&file, "x").unwrap();
        let err = ensure_private_dir(&file).unwrap_err().to_string();
        assert!(err.contains("not a directory"), "{err}");
    }

    #[test]
    fn a_symlinked_home_resolves_to_one_spelling() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(canonical_home(link), real.canonicalize().unwrap());
        let missing = tmp.path().join("not-yet");
        assert_eq!(
            canonical_home(missing.clone()),
            tmp.path().canonicalize().unwrap().join("not-yet"),
            "a new home uses its canonical parent"
        );
    }

    #[test]
    fn a_new_home_under_a_symlink_keeps_its_socket_after_creation() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let alias = tmp.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let home = alias.join("h".repeat(120));
        let before = canonical_home(home.clone());
        std::fs::create_dir(&home).unwrap();
        assert_eq!(before, canonical_home(home));
        assert_eq!(
            paths::socket_dir(&before),
            paths::socket_dir(&real.join("h".repeat(120)).canonicalize().unwrap())
        );
    }

    #[test]
    fn socket_dir_ready_uses_the_home_when_it_fits_and_a_private_fallback_otherwise() {
        let tmp = tempfile::tempdir().unwrap();
        let short = tmp.path().join("h");
        assert_eq!(socket_dir_ready(&short).unwrap(), short);
        assert!(short.is_dir());

        let long = tmp.path().join("x".repeat(paths::SOCKET_PATH_MAX));
        std::fs::create_dir_all(&long).unwrap();
        let dir = socket_dir_ready(&long).unwrap();
        assert_ne!(dir, long);
        assert!(paths::fits_socket(&dir.join("container.sock")));
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
