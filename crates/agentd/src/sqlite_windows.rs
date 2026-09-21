//! Permanent Windows SQLite creation policy, installed before connections.
//!
//! SQLite opens WAL, journal and shared-memory files itself. Its win32 VFS
//! routes those opens through the CreateFileW syscall slot, including the
//! shared-memory path that does not pass through the VFS xOpen method.

use std::ffi::CStr;
use std::sync::OnceLock;

use anyhow::{Context, Result, ensure};
use rusqlite::ffi;

// Failure is permanent too: callers must not race a retry against connections
// that have already passed this gate. Never reset the slot or shut SQLite down.
static INITIALIZED: OnceLock<Result<(), String>> = OnceLock::new();

pub(crate) fn initialize() -> Result<()> {
    INITIALIZED
        .get_or_init(|| install().map_err(|error| format!("{error:#}")))
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))
        .copied()
}

fn install() -> Result<()> {
    agentdocker_host::dirs::initialize_sqlite_protection()
        .context("cannot prepare private SQLite file security")?;

    // SAFETY: every AgentDocker connection (including raw test fixtures) waits
    // for INITIALIZED, and both binaries call this before creating workers.
    // SQLite initialization is thread safe; the syscall table mutation below
    // happens once, before any of our connections can use it.
    let result = unsafe { ffi::sqlite3_initialize() };
    ensure!(
        result == ffi::SQLITE_OK,
        "cannot initialize SQLite's Windows storage platform: {result}"
    );
    // SAFETY: SQLite is initialized; a null name selects its default VFS.
    let vfs = unsafe { ffi::sqlite3_vfs_find(std::ptr::null()) };
    ensure!(!vfs.is_null(), "SQLite has no default Windows VFS");
    // SAFETY: the returned VFS is valid and stays registered for this process.
    ensure!(
        unsafe { (*vfs).iVersion } >= 3,
        "SQLite's default Windows VFS lacks syscall overrides"
    );
    // SAFETY: the version check above establishes the version-3 VFS layout.
    let vfs_ref = unsafe { &*vfs };
    ensure!(
        !vfs_ref.zName.is_null() && unsafe { CStr::from_ptr(vfs_ref.zName) }.to_bytes() == b"win32",
        "SQLite's default VFS is not win32"
    );
    let get = vfs_ref
        .xGetSystemCall
        .context("SQLite's win32 VFS has no syscall getter")?;
    let set = vfs_ref
        .xSetSystemCall
        .context("SQLite's win32 VFS has no syscall setter")?;
    // SAFETY: the verified VFS getter accepts the static, terminated name.
    ensure!(
        unsafe { get(vfs, c"CreateFileW".as_ptr()) }.is_some(),
        "SQLite's win32 VFS has no CreateFileW syscall"
    );

    // First coerce the function item to its actual Win32 function-pointer
    // signature (inferred from the host API, including SECURITY_ATTRIBUTES).
    // SQLite stores syscalls as erased C function pointers and casts this slot
    // back to the exact CreateFileW/system calling convention before invoking.
    let callback: unsafe extern "system" fn(_, _, _, _, _, _, _) -> _ =
        agentdocker_host::dirs::sqlite_create_file;
    // SAFETY: this only erases a function pointer; it is never called with the
    // erased signature. The verified CreateFileW slot restores its Win32 ABI.
    let erased = unsafe {
        std::mem::transmute::<
            unsafe extern "system" fn(_, _, _, _, _, _, _) -> _,
            unsafe extern "C" fn(),
        >(callback)
    };
    // SAFETY: this is the only write to the global syscall table, serialized
    // by INITIALIZED before all connection opens. The callback lives forever.
    let result = unsafe { set(vfs, c"CreateFileW".as_ptr(), Some(erased)) };
    ensure!(
        result == ffi::SQLITE_OK,
        "cannot install SQLite's private CreateFileW syscall: {result}"
    );
    // SAFETY: the getter and VFS remain valid after replacement of one syscall.
    let installed = unsafe { get(vfs, c"CreateFileW".as_ptr()) }
        .context("SQLite lost its CreateFileW syscall after installation")?;
    ensure!(
        std::ptr::fn_addr_eq(installed, erased),
        "SQLite did not retain its private CreateFileW syscall"
    );
    Ok(())
}
