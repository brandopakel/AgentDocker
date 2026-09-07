//! App-owned Windows state: protected ACLs at creation, checked handles for
//! existing state, and no traversal of a final reparse point or hard-linked file.
use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::{
    ffi::OsStrExt,
    io::{AsRawHandle, FromRawHandle, OwnedHandle},
};
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::{
    Foundation::{
        GENERIC_ALL, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
    },
    Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL,
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT, SetSecurityInfo,
        },
        DACL_SECURITY_INFORMATION, GetAce, GetSecurityDescriptorDacl, GetTokenInformation,
        INHERIT_ONLY_ACE, IsValidSid, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    },
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateDirectoryW, CreateFileW, DELETE,
        FILE_APPEND_DATA, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_DELETE_CHILD, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA,
        GetFileInformationByHandle, OPEN_EXISTING, READ_CONTROL, WRITE_DAC, WRITE_OWNER,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

struct LocalAllocation(*mut c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: each pointer was allocated by a documented LocalAlloc API.
        unsafe {
            LocalFree(self.0);
        }
    }
}

fn denied(detail: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, detail)
}

fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut value: Vec<_> = path.as_os_str().encode_wide().collect();
    if value.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains NUL",
        ));
    }
    value.push(0);
    Ok(value)
}

/// Caller supplies a valid SID from a token/security descriptor, or a bounded
/// ACL entry checked below. The allocated string is released on every path.
unsafe fn sid_text(sid: PSID) -> io::Result<String> {
    if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
        return Err(denied("invalid security identifier"));
    }
    let mut text = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _allocated = LocalAllocation(text.cast());
    let mut length = 0;
    // A converted SID is short ASCII, NUL-terminated by Windows.
    while length < 256 && unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    if length == 256 {
        return Err(denied("security identifier exceeds its bound"));
    }
    String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
        .map_err(|_| denied("security identifier is not valid Unicode"))
}

pub(crate) fn current_sid() -> io::Result<String> {
    let mut token = null_mut();
    // SAFETY: pseudohandle is valid; successful OpenProcessToken transfers a
    // real handle, immediately owned below. No privilege changes are requested.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    token_sid(token.as_raw_handle())
}

fn token_sid(token: HANDLE) -> io::Result<String> {
    let mut bytes = 0;
    unsafe {
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut bytes);
    }
    if bytes < std::mem::size_of::<TOKEN_USER>() as u32 || bytes > 64 * 1024 {
        return Err(denied("unexpected token size"));
    }
    // usize alignment satisfies TOKEN_USER and the SID stored behind it.
    let mut buffer = vec![0usize; (bytes as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            bytes,
            &mut bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    unsafe { sid_text(user.User.Sid) }
}

/// Query an actual process token; never infer ownership from an executable or
/// accept an unavailable owner as the current user.
pub(crate) fn process_sid(pid: u32) -> io::Result<String> {
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    let mut token = null_mut();
    if unsafe { OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    token_sid(token.as_raw_handle())
}

struct Protection {
    sid: String,
    descriptor: LocalAllocation,
}
impl Protection {
    fn new() -> io::Result<Self> {
        let sid = current_sid()?;
        // Private to the owning user and SYSTEM, inherited by child state.
        let sddl: Vec<_> = format!("O:{sid}D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{sid})")
            .encode_utf16()
            .chain([0])
            .collect();
        let mut descriptor = null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            sid,
            descriptor: LocalAllocation(descriptor),
        })
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.descriptor.0,
            bInheritHandle: 0,
        }
    }

    /// Existing state may have broad read access that can be narrowed. Refuse
    /// foreign ownership or untrusted write access before changing any ACL.
    fn validate_access(&self, handle: HANDLE, ancestor: bool) -> io::Result<()> {
        let mut owner = null_mut();
        let mut acl = null_mut();
        let mut descriptor = null_mut();
        let code = unsafe {
            GetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                null_mut(),
                &mut acl,
                null_mut(),
                &mut descriptor,
            )
        };
        if code != 0 {
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        let _allocated = LocalAllocation(descriptor);
        let owner = unsafe { sid_text(owner)? };
        if owner != self.sid && !(ancestor && trusted_system(&owner)) {
            return Err(denied(
                "state or ancestor belongs to an untrusted Windows principal",
            ));
        }
        if acl.is_null() {
            return Err(denied("state has an unrestricted DACL"));
        }
        let writable = if ancestor {
            GENERIC_ALL | GENERIC_WRITE | DELETE | WRITE_DAC | WRITE_OWNER | FILE_DELETE_CHILD
        } else {
            GENERIC_ALL
                | GENERIC_WRITE
                | DELETE
                | WRITE_DAC
                | WRITE_OWNER
                | FILE_WRITE_DATA
                | FILE_APPEND_DATA
                | FILE_WRITE_EA
                | FILE_WRITE_ATTRIBUTES
                | FILE_DELETE_CHILD
        };
        let count = unsafe { (*acl).AceCount };
        for index in 0..count {
            let mut entry = null_mut();
            if unsafe { GetAce(acl, index as u32, &mut entry) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let header = unsafe { &*entry.cast::<ACE_HEADER>() };
            if header.AceFlags as u32 & INHERIT_ONLY_ACE != 0 {
                continue;
            }
            match header.AceType {
                1 => continue, // ACCESS_DENIED_ACE does not grant write access.
                0 => (),       // ACCESS_ALLOWED_ACE
                _ => return Err(denied("state has an unsupported access-control entry")),
            }
            let sid_offset = std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart);
            if (header.AceSize as usize) < sid_offset + 8 {
                return Err(denied("truncated access-control entry"));
            }
            let allowed = unsafe { &*entry.cast::<ACCESS_ALLOWED_ACE>() };
            if allowed.Mask & writable == 0 {
                continue;
            }
            let sid = unsafe { entry.cast::<u8>().add(sid_offset) };
            let subauthorities = unsafe { *sid.add(1) } as usize;
            if sid_offset + 8 + 4 * subauthorities > header.AceSize as usize {
                return Err(denied("truncated access-control identifier"));
            }
            let trustee = unsafe { sid_text(sid.cast())? };
            // Administrators can already take ownership, as root can on Unix.
            if trustee != self.sid && !trusted_system(&trustee) {
                return Err(denied("state is writable by another Windows principal"));
            }
        }
        Ok(())
    }

    fn validate_and_narrow(&self, handle: HANDLE) -> io::Result<()> {
        self.validate_access(handle, false)?;
        let mut present = 0;
        let mut defaulted = 0;
        let mut private_acl: *mut ACL = null_mut();
        if unsafe {
            GetSecurityDescriptorDacl(
                self.descriptor.0,
                &mut present,
                &mut private_acl,
                &mut defaulted,
            )
        } == 0
            || present == 0
            || private_acl.is_null()
        {
            return Err(denied("private DACL is unavailable"));
        }
        let code = unsafe {
            SetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                private_acl,
                null(),
            )
        };
        if code != 0 {
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        Ok(())
    }
}

// These principals administer the machine and can already take ownership.
fn trusted_system(sid: &str) -> bool {
    matches!(
        sid,
        "S-1-5-18"
            | "S-1-5-32-544"
            | "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
    )
}

/// Check the existing ancestry without changing user directories. Retain handles
/// without delete sharing until the state operation finishes, so a directory
/// cannot be renamed out from under the verified path. Creation rights alone do
/// not authorize replacing an existing protected child; DELETE_CHILD does.
fn guard_ancestors(path: &Path, protection: &Protection) -> io::Result<Vec<File>> {
    if !path.is_absolute() {
        return Err(denied("Windows state paths must be absolute"));
    }
    let mut paths: Vec<_> = path.ancestors().skip(1).collect();
    paths.reverse();
    let mut guards = Vec::new();
    for ancestor in paths {
        let raw = wide(ancestor)?;
        let handle = unsafe {
            CreateFileW(
                raw.as_ptr(),
                READ_CONTROL | FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                break; // The remaining directories will be privately created.
            }
            return Err(error);
        }
        let file = unsafe { File::from_raw_handle(handle) };
        check_kind(&file, true)?;
        protection.validate_access(file.as_raw_handle(), true)?;
        guards.push(file);
    }
    Ok(guards)
}

fn check_kind(file: &File, directory: bool) -> io::Result<()> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory
        || (!directory && info.nNumberOfLinks != 1)
    {
        return Err(denied(
            "state must have the expected type without reparse points or file hard links",
        ));
    }
    Ok(())
}

fn open(
    path: &Path,
    protection: &Protection,
    directory: bool,
    create: bool,
    append: bool,
) -> io::Result<File> {
    let path = wide(path)?;
    let attributes = protection.attributes();
    let access = READ_CONTROL
        | WRITE_DAC
        | if directory {
            FILE_READ_ATTRIBUTES
        } else if append {
            FILE_GENERIC_READ | FILE_APPEND_DATA
        } else {
            GENERIC_READ | GENERIC_WRITE
        };
    let flags = FILE_FLAG_OPEN_REPARSE_POINT
        | if directory {
            FILE_FLAG_BACKUP_SEMANTICS
        } else {
            FILE_ATTRIBUTE_NORMAL
        };
    let disposition = if create { CREATE_NEW } else { OPEN_EXISTING };
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | if directory { 0 } else { FILE_SHARE_DELETE },
            &attributes,
            disposition,
            flags,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_handle(handle) };
    check_kind(&file, directory)?;
    protection.validate_and_narrow(file.as_raw_handle())?;
    Ok(file)
}

pub fn private_file(path: &Path, create: bool, append: bool) -> io::Result<File> {
    let protection = Protection::new()?;
    let _ancestors = guard_ancestors(path, &protection)?;
    if create {
        match open(path, &protection, false, true, append) {
            Ok(file) => return Ok(file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error),
        }
    }
    open(path, &protection, false, false, append)
}

fn create_directory(
    path: &Path,
    protection: &Protection,
    guards: &mut Vec<File>,
) -> io::Result<()> {
    let raw = wide(path)?;
    let attributes = protection.attributes();
    if unsafe { CreateDirectoryW(raw.as_ptr(), &attributes) } == 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .ok_or(error)?;
            create_directory(parent, protection, guards)?;
            if unsafe { CreateDirectoryW(raw.as_ptr(), &attributes) } == 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
        } else if error.kind() != io::ErrorKind::AlreadyExists {
            return Err(error);
        }
    }
    // Includes a competing creation: validate before creating anything below it.
    guards.push(open(path, protection, true, false, false)?);
    Ok(())
}

pub fn secure_state_dir(path: &Path) -> io::Result<()> {
    let protection = Protection::new()?;
    let mut ancestors = guard_ancestors(path, &protection)?;
    create_directory(path, &protection, &mut ancestors)?;
    Ok(())
}

pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    secure_state_dir(path)
}

/// Named-pipe endpoints are local OS names, not filesystem paths. Peer and
/// pipe-DACL verification belongs to the IPC adapter before sending requests.
pub fn check_socket_parent(path: &Path) -> io::Result<()> {
    let text = path
        .to_str()
        .ok_or_else(|| denied("pipe name must be Unicode"))?;
    let name = text
        .strip_prefix(r"\\.\pipe\")
        .ok_or_else(|| denied("a local Windows named-pipe endpoint is required"))?;
    if name.is_empty() || name.contains(['\\', '/', '\0']) || text.encode_utf16().count() > 256 {
        return Err(denied("invalid Windows named-pipe endpoint"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn acl_bytes(handle: HANDLE) -> (bool, Vec<u8>) {
        use windows_sys::Win32::Security::{GetSecurityDescriptorControl, SE_DACL_PROTECTED};
        let mut descriptor = null_mut();
        let mut acl = null_mut();
        assert_eq!(
            unsafe {
                GetSecurityInfo(
                    handle,
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    &mut acl,
                    null_mut(),
                    &mut descriptor,
                )
            },
            0
        );
        let _allocated = LocalAllocation(descriptor);
        let mut control = 0;
        let mut revision = 0;
        assert_ne!(
            unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) },
            0
        );
        assert!(!acl.is_null());
        (
            control & SE_DACL_PROTECTED != 0,
            unsafe { std::slice::from_raw_parts(acl.cast::<u8>(), (*acl).AclSize as usize) }
                .to_vec(),
        )
    }

    fn grant_everyone_write(file: &File) {
        let sddl: Vec<_> = "D:P(A;OICI;FA;;;WD)".encode_utf16().chain([0]).collect();
        let mut descriptor = null_mut();
        assert_ne!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    null_mut(),
                )
            },
            0
        );
        let _allocated = LocalAllocation(descriptor);
        let mut present = 0;
        let mut defaulted = 0;
        let mut acl = null_mut();
        assert_ne!(
            unsafe {
                GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted)
            },
            0
        );
        assert_eq!(
            unsafe {
                SetSecurityInfo(
                    file.as_raw_handle(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    acl,
                    null(),
                )
            },
            0
        );
    }

    #[test]
    fn foreign_write_acl_and_replaceable_ancestry_are_refused_without_narrowing() {
        let temporary = tempfile::tempdir().unwrap();
        let state = temporary.path().join("state");
        secure_state_dir(&state).unwrap();
        let path = state.join("journal");
        let file = private_file(&path, true, false).unwrap();
        assert!(
            acl_bytes(file.as_raw_handle()).0,
            "new state has a protected DACL"
        );
        grant_everyone_write(&file);
        let before = acl_bytes(file.as_raw_handle());
        assert!(private_file(&path, false, false).is_err());
        assert_eq!(acl_bytes(file.as_raw_handle()), before);
        let protection = Protection::new().unwrap();
        let directory = open(&state, &protection, true, false, false).unwrap();
        grant_everyone_write(&directory);
        let before = acl_bytes(directory.as_raw_handle());
        assert!(secure_state_dir(&state.join("must-not-create")).is_err());
        assert!(!state.join("must-not-create").exists());
        assert_eq!(acl_bytes(directory.as_raw_handle()), before);
    }

    #[test]
    fn state_is_private_at_creation_and_file_open_preserves_contents() {
        let temporary = tempfile::tempdir().unwrap();
        let state = temporary.path().join("state/nested");
        secure_state_dir(&state).unwrap();
        let path = state.join("journal");
        private_file(&path, true, false)
            .unwrap()
            .write_all(b"first")
            .unwrap();
        private_file(&path, false, true)
            .unwrap()
            .write_all(b" second")
            .unwrap();
        let mut contents = String::new();
        private_file(&path, false, false)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "first second");
        let linked = state.join("linked");
        std::fs::hard_link(&path, &linked).unwrap();
        assert!(private_file(&path, false, false).is_err());
        assert!(private_file(&linked, false, false).is_err());
        assert!(private_file(&state, false, false).is_err());
        assert!(secure_state_dir(&path).is_err());
    }
}
