//! Bounded policy reads. Absence is distinct from unreadability or a special file.

use std::{fs, io, io::Read, path::Path, time::SystemTime};

pub const MAX_BYTES: u64 = 1024 * 1024;

/// Identity and change metadata for an already validated regular file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stamp {
    len: u64,
    modified: SystemTime,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl Stamp {
    fn of(meta: &fs::Metadata) -> io::Result<Self> {
        if !meta.is_file() || meta.len() > MAX_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "policy must be a regular file of at most 1 MiB",
            ));
        }
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            len: meta.len(),
            modified: meta.modified()?,
            #[cfg(unix)]
            identity: (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec()),
        })
    }
}

pub enum ReadPolicy {
    Absent,
    Unchanged,
    Text { stamp: Stamp, text: String },
}

/// Check metadata, then read at most 1 MiB plus one byte. Reject symlinks and
/// special files, including a FIFO swapped in between inspection and open.
/// A changed identity during reading is an error, never an empty policy.
pub fn read_changed(path: &Path, previous: Option<&Stamp>) -> io::Result<ReadPolicy> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // ENOENT can also mean a dangling ancestor symlink. Only ordinary
            // missing components count as deliberate absence.
            for parent in path.ancestors().skip(1) {
                match fs::symlink_metadata(parent) {
                    Ok(meta) => {
                        if meta.file_type().is_symlink() {
                            fs::metadata(parent)?;
                        }
                        break;
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                }
            }
            return Ok(ReadPolicy::Absent);
        }
        Err(error) => return Err(error),
    };
    let stamp = Stamp::of(&meta)?;
    if previous == Some(&stamp) {
        return Ok(ReadPolicy::Unchanged);
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if Stamp::of(&file.metadata()?)? != stamp {
        return Err(io::Error::other("policy changed before reading"));
    }
    let mut text = String::new();
    (&file).take(MAX_BYTES + 1).read_to_string(&mut text)?;
    if text.len() as u64 > MAX_BYTES || Stamp::of(&file.metadata()?)? != stamp {
        return Err(io::Error::other("policy changed during reading"));
    }
    Ok(ReadPolicy::Text { stamp, text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_oversized_special_and_dangling_files_and_observes_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy.toml");
        assert!(matches!(
            read_changed(&path, None).unwrap(),
            ReadPolicy::Absent
        ));
        fs::write(&path, "first").unwrap();
        let ReadPolicy::Text { stamp, .. } = read_changed(&path, None).unwrap() else {
            panic!()
        };
        assert!(matches!(
            read_changed(&path, Some(&stamp)).unwrap(),
            ReadPolicy::Unchanged
        ));
        let replacement = dir.path().join("replacement");
        fs::write(&replacement, "other").unwrap();
        fs::rename(&replacement, &path).unwrap();
        assert!(
            matches!(read_changed(&path, Some(&stamp)).unwrap(), ReadPolicy::Text {text, ..} if text == "other")
        );
        fs::File::create(&path)
            .unwrap()
            .set_len(MAX_BYTES + 1)
            .unwrap();
        assert!(read_changed(&path, None).is_err());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(read_changed(&path, None).is_err());
        fs::remove_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let fifo = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
            assert!(read_changed(&path, None).is_err());
            fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(dir.path().join("missing"), &path).unwrap();
            assert!(read_changed(&path, None).is_err());
            let parent = dir.path().join("dangling");
            std::os::unix::fs::symlink(dir.path().join("missing"), &parent).unwrap();
            assert!(read_changed(&parent.join("policy.toml"), None).is_err());
        }
    }
}
