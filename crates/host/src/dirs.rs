//! Directories the daemon and its clients must agree on and be able to
//! trust: a socket directory under a shared temp root is safe to bind in
//! only if it is ours alone.

use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

use agentdocker_core::paths;

/// Create `dir` for this user alone (mode `0700`) when it is missing, and
/// refuse it when it is not a directory we own that nobody else can write
/// to. A pre-planted directory or symlink under `/tmp` fails here instead
/// of receiving our sockets.
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
    if dir == home {
        std::fs::create_dir_all(&dir)?;
    } else {
        ensure_private_dir(&dir)?;
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

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
