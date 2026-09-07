//! File reads that reject special files and final-link substitution, plus the
//! platform metadata needed to notice an edit during a bounded observation.
use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::path::Path;

pub fn open_regular(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(path)?
    };
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        // Reject devices, named pipes and reparse points before opening data.
        // OPEN_REPARSE_POINT also prevents a racing final-link substitution.
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::other(
                "a regular file without reparse points is required",
            ));
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::other("file was replaced by a reparse point"));
        }
        file
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("a regular file is required"));
    }
    Ok(file)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stamp {
    length: u64,
    modified: i128,
    changed: i128,
    attributes: u32,
}

pub fn stamp(file: &File) -> io::Result<Stamp> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        Ok(Stamp {
            length: metadata.len(),
            modified: metadata.mtime() as i128 * 1_000_000_000 + metadata.mtime_nsec() as i128,
            changed: metadata.ctime() as i128 * 1_000_000_000 + metadata.ctime_nsec() as i128,
            attributes: metadata.mode(),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_BASIC_INFO, FILE_STANDARD_INFO, FileBasicInfo, FileStandardInfo,
            GetFileInformationByHandleEx,
        };
        let mut basic = FILE_BASIC_INFO::default();
        let mut standard = FILE_STANDARD_INFO::default();
        // SAFETY: both buffers have the size and alignment required by their
        // information class; file keeps the queried handle alive.
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileBasicInfo,
                (&mut basic as *mut FILE_BASIC_INFO).cast(),
                std::mem::size_of_val(&basic) as u32,
            )
        } == 0
            || unsafe {
                GetFileInformationByHandleEx(
                    file.as_raw_handle(),
                    FileStandardInfo,
                    (&mut standard as *mut FILE_STANDARD_INFO).cast(),
                    std::mem::size_of_val(&standard) as u32,
                )
            } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Stamp {
            length: standard.EndOfFile as u64,
            modified: basic.LastWriteTime as i128,
            changed: basic.ChangeTime as i128,
            attributes: basic.FileAttributes,
        })
    }
}

/// Native Windows observations track the read-only attribute. Windows has no
/// Unix executable permission bits; snapshots are interpreted on their host.
pub fn observed_permissions(metadata: &Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.mode() & 0o111
    }
    #[cfg(windows)]
    {
        u32::from(metadata.permissions().readonly())
    }
}

/// Windows build contexts normalize files to 644 (444 if read-only). Dockerfile
/// chmod/COPY --chmod defines container executable bits on those hosts.
pub fn context_mode(metadata: &Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o777
    }
    #[cfg(windows)]
    {
        if metadata.permissions().readonly() {
            0o444
        } else {
            0o644
        }
    }
}

pub fn set_context_mode(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }
    #[cfg(windows)]
    {
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_readonly(mode & 0o222 == 0);
        std::fs::set_permissions(path, permissions)
    }
}
