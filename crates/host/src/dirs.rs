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
