//! File reads that reject special files and final-link substitution, plus the
//! platform metadata needed to notice an edit during a bounded observation.
use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::path::Path;

/// Publish an already-flushed staged file in the same directory. Unix
/// syncs the containing directory after rename; Windows requests a
/// write-through move rather than trying to open a directory as a file.
pub fn publish_staged(staged: &Path, destination: &Path) -> io::Result<()> {
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if parent.is_none() || staged.parent() != parent {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "staged file must share its destination directory",
        ));
    }
    #[cfg(unix)]
    {
        std::fs::rename(staged, destination)?;
        File::open(parent.expect("validated parent"))?.sync_all()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        let wide = |path: &Path| -> io::Result<Vec<u16>> {
            let mut name: Vec<u16> = path.as_os_str().encode_wide().collect();
            if name.contains(&0) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "path contains a NUL",
                ));
            }
            name.push(0);
            Ok(name)
        };
        let staged = wide(staged)?;
        let destination = wide(destination)?;
        // SAFETY: both paths are nul-terminated and remain live through
        // the call; no cross-volume copy or deferred rename is allowed.
        if unsafe {
            MoveFileExW(
                staged.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Publish an atomic record while readers retain the prior file's handle.
/// Windows' POSIX rename semantics explicitly preserve those handles; the
/// legacy MoveFileEx replacement used for closed files can deny this case.
/// The staged record must already be flushed and share its destination's
/// directory. Unsupported filesystems fail without a delete/copy fallback.
pub fn publish_snapshot(staged: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        publish_staged(staged, destination)
    }
    #[cfg(windows)]
    {
        use std::os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle};
        use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
        use windows_sys::Win32::Storage::FileSystem::{
            DELETE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_RENAME_INFO, FileRenameInfoEx,
            SetFileInformationByHandle,
        };
        let parent = destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty());
        if parent.is_none() || staged.parent() != parent {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "snapshot must share its destination directory",
            ));
        }
        let name: Vec<u16> = std::path::absolute(destination)?
            .as_os_str()
            .encode_wide()
            .collect();
        if name.is_empty() || name.len() > 32767 || name.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid snapshot destination",
            ));
        }
        // The record is private and previously flushed by its writer. Keep
        // final reparse points and special files out of this publication path.
        let checked = open_regular(staged)?;
        let file = OpenOptions::new()
            .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(staged)?;
        if same_file::Handle::from_file(checked)?
            != same_file::Handle::from_file(file.try_clone()?)?
        {
            return Err(io::Error::other(
                "staged snapshot changed before publication",
            ));
        }
        file.sync_all()?;
        // FILE_RENAME_INFO's alignment is at most a pointer's. Extra storage
        // includes its trailing variable UTF-16 name and a terminating zero.
        let bytes = std::mem::size_of::<FILE_RENAME_INFO>() + (name.len() + 1) * 2;
        let mut storage = vec![0_usize; bytes.div_ceil(std::mem::size_of::<usize>())];
        let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        // SDK FILE_RENAME_FLAG_REPLACE_IF_EXISTS | FILE_RENAME_FLAG_POSIX_SEMANTICS.
        // https://learn.microsoft.com/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information
        const REPLACE_OPEN_RECORD: u32 = 0x1 | 0x2;
        // SAFETY: storage is aligned, zero-initialized and large enough for the
        // header/name. The handle and buffer remain live through the call.
        let renamed = unsafe {
            (*info).Anonymous.Flags = REPLACE_OPEN_RECORD;
            (*info).FileNameLength = (name.len() * 2) as u32;
            std::ptr::copy_nonoverlapping(
                name.as_ptr(),
                std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
                name.len(),
            );
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileRenameInfoEx,
                info.cast(),
                bytes as u32,
            )
        };
        if renamed == 0 {
            return Err(io::Error::last_os_error());
        }
        file.sync_all()
    }
}

#[cfg(test)]
mod publication_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn a_flushed_report_is_published_and_replaced_in_its_own_directory() {
        let directory = tempfile::tempdir().unwrap();
        let staged = directory.path().join("session.exit.staging");
        let destination = directory.path().join("session.exit");
        for bytes in [
            b"first report".as_slice(),
            b"second complete report".as_slice(),
        ] {
            let mut file = File::create(&staged).unwrap();
            file.write_all(bytes).unwrap();
            file.sync_all().unwrap();
            drop(file);
            publish_staged(&staged, &destination).unwrap();
            assert_eq!(std::fs::read(&destination).unwrap(), bytes);
            assert!(!staged.exists());
        }
    }
}

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

/// What tells one file from another on its filesystem, whatever it is
/// named: device and inode on Unix, volume serial number and file index
/// on Windows. Read from an open handle, so it is the identity of the
/// file that was opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub device: u64,
    pub inode: u64,
}

pub fn identity(file: &File) -> io::Result<Identity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        Ok(Identity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: the buffer is the size the call writes; file keeps the
        // handle alive.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Identity {
            device: u64::from(info.dwVolumeSerialNumber),
            inode: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
