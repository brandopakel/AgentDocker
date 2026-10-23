//! Seeding a new worktree with the build output of the checkout it was made
//! from (`worktree_create`'s `seed_build`), so its first build starts warm.
//!
//! Every step goes through directory descriptors, never through a path
//! that could be resolved again: each part of a name is opened with
//! `O_NOFOLLOW | O_DIRECTORY` relative to the one before it, so a folder
//! renamed and replaced by a symlink at any moment is either refused or
//! still the folder that was checked, and nothing is read from or written
//! to anywhere else. No symlink on the way to a named directory is
//! followed, in the source or in the new worktree.
//!
//! The seed is built under a fresh name of its own inside the destination's
//! checked folder and only then published under its real name by a rename
//! that never replaces anything (`renameatx_np(RENAME_EXCL)` on macOS,
//! `renameat2(RENAME_NOREPLACE)` on Linux; a filesystem without one is
//! refused rather than risk replacing an empty folder another writer has
//! just made and holds open): when another writer made that
//! name meanwhile, theirs is left as it was and the seed is removed. Only
//! what this seed made is ever removed, and only while it is still what was
//! made.
//!
//! On macOS the whole directory is cloned from its open descriptor with
//! `fclonefileat(2)`: on APFS that is copy-on-write, near-instant, and
//! takes no space until one side changes a file. Where that cannot be done
//! it is copied file by file instead, each file by reflink where Linux's
//! filesystem shares extents (`FICLONE`) and byte for byte otherwise, in
//! chunks that stop at the time limit and before the copy has written half
//! the space that was free when it began. Permissions and times are kept,
//! so a build tool that compares times rebuilds what the new checkout
//! changed rather than trusting output newer than its sources; symlinks
//! inside are copied as symlinks.
//!
//! Nothing here fails the worktree: each directory gets an [`Outcome`]. A
//! clone is one system call, which is not interrupted; the time limit
//! bounds the copies. Windows has no such descriptor-relative copy here
//! yet, so it seeds nothing and says so.

use std::path::Path;
use std::time::{Duration, Instant};

use agentdocker_core::seed::{Outcome, Seeded};

/// Seed `worktree` with each of `names` from `source`, all within `limit`.
/// Both are checkout roots; the names are checked again here, so a caller
/// that skipped [`agentdocker_core::seed::check`] still gets nothing
/// outside them.
pub fn seed(source: &Path, worktree: &Path, names: &[String], limit: Duration) -> Vec<Seeded> {
    let options = Options {
        deadline: Instant::now() + limit,
        clone: true,
        budget: None,
    };
    seed_with(source, worktree, names, &options)
}

#[cfg_attr(not(unix), allow(dead_code))]
struct Options {
    deadline: Instant,
    /// Try copy-on-write first (`fclonefileat`, `FICLONE`). Tests turn it
    /// off to reach the plain copy on a filesystem that clones.
    clone: bool,
    /// The bytes a plain copy may write in all; `None` is half the space
    /// free where it goes.
    budget: Option<u64>,
}

#[cfg(unix)]
fn seed_with(source: &Path, worktree: &Path, names: &[String], options: &Options) -> Vec<Seeded> {
    let roots = unix::root(source).and_then(|s| Ok((s, unix::root(worktree)?)));
    names
        .iter()
        .map(|name| {
            let (outcome, note) = match &roots {
                Ok((source, worktree)) => unix::one(source, worktree, name, options),
                Err(e) => (Outcome::Failed, Some(e.to_string())),
            };
            Seeded {
                dir: name.clone(),
                outcome,
                note,
            }
        })
        .collect()
}

#[cfg(not(unix))]
fn seed_with(
    _source: &Path,
    _worktree: &Path,
    names: &[String],
    _options: &Options,
) -> Vec<Seeded> {
    names
        .iter()
        .map(|name| Seeded {
            dir: name.clone(),
            outcome: Outcome::Refused,
            note: Some(
                "not seeded on this platform yet: there is no copy here that a link swapped in \
                 cannot redirect"
                    .into(),
            ),
        })
        .collect()
}

#[cfg(unix)]
mod unix {
    use std::ffi::{CStr, CString};
    use std::fs::File;
    use std::io::{self, Read};
    use std::mem::MaybeUninit;
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    use agentdocker_core::seed::{self, Outcome};

    use super::Options;

    type Answer = (Outcome, Option<String>);
    /// Device and inode: what a folder is, whatever it is called now.
    type Identity = (u64, u64);

    /// The deepest a copy goes; each level holds two descriptors open.
    const MAX_DEPTH: usize = 64;
    /// The deepest a removal goes; each level holds one descriptor open.
    const REMOVE_DEPTH: usize = 128;
    /// A plain copy looks at its time and budget at least this often.
    const CHUNK: u64 = 8 * 1024 * 1024;
    /// The budget when the filesystem will not say how much is free.
    const UNKNOWN_FREE_BUDGET: u64 = 1024 * 1024 * 1024;

    /// Where a seed is between its checks and its publication, for tests
    /// that change the filesystem under it there.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Phase {
        /// Both sides are open and checked; nothing is made yet.
        Opened,
        /// The seed is built under its temporary name, not yet published.
        Built,
    }

    /// What a test runs at each phase, on its own thread.
    #[cfg(test)]
    pub(super) type Hook = Box<dyn FnMut(Phase)>;

    #[cfg(test)]
    thread_local! {
        pub(super) static PAUSE: std::cell::RefCell<Option<Hook>> =
            const { std::cell::RefCell::new(None) };
    }

    #[cfg(test)]
    thread_local! {
        /// Whether a test has this filesystem lack an exclusive rename.
        pub(super) static NO_EXCLUSIVE_RENAME: std::cell::Cell<bool> =
            const { std::cell::Cell::new(false) };
    }

    fn pause(phase: Phase) {
        #[cfg(test)]
        PAUSE.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook(phase);
            }
        });
        #[cfg(not(test))]
        let _ = phase;
    }

    fn check(status: libc::c_int) -> io::Result<()> {
        if status == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn cstr(bytes: &[u8]) -> io::Result<CString> {
        CString::new(bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a name with a NUL in it"))
    }

    /// A checkout root, opened by its canonical path without following its
    /// last part.
    pub(super) fn root(path: &Path) -> io::Result<OwnedFd> {
        let path = cstr(std::fs::canonicalize(path)?.as_os_str().as_bytes())?;
        open_dir(libc::AT_FDCWD, &path)
    }

    /// The directory `name` in `at`, never through a symlink.
    fn open_dir(at: RawFd, name: &CStr) -> io::Result<OwnedFd> {
        // SAFETY: a valid C string, and `at` an open directory or AT_FDCWD.
        let fd = unsafe {
            libc::openat(
                at,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh descriptor nothing else owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// What `name` in `at` is: a symlink itself, not what it names.
    fn lstat(at: RawFd, name: &CStr) -> io::Result<libc::stat> {
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: a valid C string and a buffer of the size the call fills.
        check(unsafe {
            libc::fstatat(
                at,
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        })?;
        // SAFETY: filled by the successful call above.
        Ok(unsafe { stat.assume_init() })
    }

    fn fstat(fd: RawFd) -> io::Result<libc::stat> {
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: an open descriptor and a buffer of the size the call fills.
        check(unsafe { libc::fstat(fd, stat.as_mut_ptr()) })?;
        // SAFETY: filled by the successful call above.
        Ok(unsafe { stat.assume_init() })
    }

    // The field types differ by platform; every one fits in a u64.
    #[allow(clippy::unnecessary_cast)]
    fn identity(stat: &libc::stat) -> Identity {
        (stat.st_dev as u64, stat.st_ino as u64)
    }

    /// The identity of the directory `name` in `at`, opened without
    /// following it.
    fn identity_at(at: RawFd, name: &CStr) -> io::Result<Identity> {
        let dir = open_dir(at, name)?;
        Ok(identity(&fstat(dir.as_raw_fd())?))
    }

    fn kind(stat: &libc::stat) -> libc::mode_t {
        stat.st_mode & libc::S_IFMT
    }

    fn mkdir(at: RawFd, name: &CStr, mode: libc::mode_t) -> io::Result<()> {
        // SAFETY: a valid C string and an open directory.
        check(unsafe { libc::mkdirat(at, name.as_ptr(), mode) })
    }

    fn unlink(at: RawFd, name: &CStr, folder: bool) -> io::Result<()> {
        let flags = if folder { libc::AT_REMOVEDIR } else { 0 };
        // SAFETY: a valid C string and an open directory.
        check(unsafe { libc::unlinkat(at, name.as_ptr(), flags) })
    }

    /// The names in an open directory, `.` and `..` left out.
    fn list(dir: &OwnedFd) -> io::Result<Vec<CString>> {
        // The stream owns the descriptor it is given: give it a copy.
        let copy = dir.try_clone()?.into_raw_fd();
        // SAFETY: an open directory descriptor, the stream's from here.
        let stream = unsafe { libc::fdopendir(copy) };
        if stream.is_null() {
            let error = io::Error::last_os_error();
            // SAFETY: the copy is still ours when no stream was made.
            unsafe { libc::close(copy) };
            return Err(error);
        }
        // SAFETY: an open stream; the copy shares its offset, so start over.
        unsafe { libc::rewinddir(stream) };
        let mut names = Vec::new();
        let listed = loop {
            nix::errno::Errno::clear();
            // SAFETY: an open stream.
            let entry = unsafe { libc::readdir(stream) };
            if entry.is_null() {
                break match nix::errno::Errno::last_raw() {
                    0 => Ok(names),
                    raw => Err(io::Error::from_raw_os_error(raw)),
                };
            }
            // SAFETY: an entry with a NUL-terminated name, valid until the
            // next call on the stream.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if name.to_bytes() != b"." && name.to_bytes() != b".." {
                names.push(name.to_owned());
            }
        };
        // SAFETY: closes the stream and the copy it owns.
        unsafe { libc::closedir(stream) };
        listed
    }

    /// Remove the directory `name` in `at` with everything in it, never
    /// through a symlink, and only while it is still `expected`.
    fn remove_tree(at: RawFd, name: &CStr, expected: Identity) -> io::Result<()> {
        let dir = open_dir(at, name)?;
        if identity(&fstat(dir.as_raw_fd())?) != expected {
            return Err(io::Error::other("it changed under the seed and was left"));
        }
        empty(&dir, 0)?;
        drop(dir);
        unlink(at, name, true)
    }

    fn empty(dir: &OwnedFd, depth: usize) -> io::Result<()> {
        if depth >= REMOVE_DEPTH {
            return Err(io::Error::other("too deep to remove"));
        }
        // A read-only folder of a clone or copy would keep its entries.
        // SAFETY: an open directory this seed made.
        check(unsafe { libc::fchmod(dir.as_raw_fd(), 0o700) })?;
        for name in list(dir)? {
            if kind(&lstat(dir.as_raw_fd(), &name)?) == libc::S_IFDIR {
                empty(&open_dir(dir.as_raw_fd(), &name)?, depth + 1)?;
                unlink(dir.as_raw_fd(), &name, true)?;
            } else {
                unlink(dir.as_raw_fd(), &name, false)?;
            }
        }
        Ok(())
    }

    fn leftover(removed: io::Result<()>) -> String {
        match removed {
            Ok(()) => String::new(),
            Err(e) => format!("; what it made could not all be removed ({e})"),
        }
    }

    /// A name in the destination's folder that only this seed uses.
    fn temporary() -> CString {
        static COUNT: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let name = format!(
            ".agentdocker-seed-{}-{nanos}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        );
        CString::new(name).expect("no NUL in a formatted name")
    }

    /// Seed `name` from the checkout open as `source` into the one open as
    /// `worktree`.
    pub(super) fn one(
        source: &OwnedFd,
        worktree: &OwnedFd,
        name: &str,
        options: &Options,
    ) -> Answer {
        let refused = |why: &str| (Outcome::Refused, Some(why.to_owned()));
        let relative = match seed::relative(Path::new(name)) {
            Ok(relative) => relative,
            Err(why) => return refused(why),
        };
        let parts = match relative
            .iter()
            .map(|part| cstr(part.as_bytes()))
            .collect::<io::Result<Vec<_>>>()
        {
            Ok(parts) => parts,
            Err(e) => return refused(&e.to_string()),
        };
        let Some((leaf, folders)) = parts.split_last() else {
            return refused("names no directory");
        };
        let from = match open_source(source, &parts) {
            Ok(from) => from,
            Err(answer) => return answer,
        };
        let mut made = Vec::new();
        let answer = match open_destination(worktree, folders, &mut made) {
            Ok(parent) => seed_into(&from, &parent, leaf, options),
            Err(answer) => answer,
        };
        if !answer.0.seeded() {
            undo(made);
        }
        answer
    }

    /// The source directory, each part opened from the one before it.
    fn open_source(source: &OwnedFd, parts: &[CString]) -> Result<OwnedFd, Answer> {
        let failed = |e: io::Error| (Outcome::Failed, Some(e.to_string()));
        let mut at = source.try_clone().map_err(failed)?;
        for part in parts {
            at = match open_dir(at.as_raw_fd(), part) {
                Ok(next) => next,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    return Err((
                        Outcome::Skipped,
                        Some("the source checkout has no such directory".into()),
                    ));
                }
                Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => {
                    let link =
                        lstat(at.as_raw_fd(), part).is_ok_and(|stat| kind(&stat) == libc::S_IFLNK);
                    let why = if link {
                        "the source has a symlink on the way; none is followed"
                    } else {
                        "not a directory in the source"
                    };
                    return Err((Outcome::Refused, Some(why.into())));
                }
                Err(e) => return Err(failed(e)),
            };
        }
        Ok(at)
    }

    /// A folder a seed made on its way: the folder it is in, its name, and
    /// what it is.
    type Made = (OwnedFd, CString, Identity);

    /// The destination's folder, each part opened from the one before it
    /// and made where missing. What it makes is recorded in `made`, so a
    /// seed that comes to nothing can take it away again.
    fn open_destination(
        worktree: &OwnedFd,
        folders: &[CString],
        made: &mut Vec<Made>,
    ) -> Result<OwnedFd, Answer> {
        let failed = |e: io::Error| (Outcome::Failed, Some(e.to_string()));
        let mut at = worktree.try_clone().map_err(failed)?;
        for part in folders {
            let created = match mkdir(at.as_raw_fd(), part, 0o777) {
                Ok(()) => true,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
                Err(e) => return Err(failed(e)),
            };
            let next = open_dir(at.as_raw_fd(), part).map_err(|_| {
                (
                    Outcome::Refused,
                    Some("the new worktree has a symlink or a file on the way".to_owned()),
                )
            })?;
            if created {
                let id = identity(&fstat(next.as_raw_fd()).map_err(failed)?);
                made.push((at, part.clone(), id));
            }
            at = next;
        }
        Ok(at)
    }

    /// Take away the folders a seed that came to nothing made on its way,
    /// deepest first, while each is still the one it made and empty.
    fn undo(made: Vec<Made>) {
        for (parent, name, id) in made.into_iter().rev() {
            if identity_at(parent.as_raw_fd(), &name).is_ok_and(|now| now == id) {
                // Only an empty folder goes; one somebody filled stays.
                let _ = unlink(parent.as_raw_fd(), &name, true);
            }
        }
    }

    /// Build the seed under a temporary name in `parent` and publish it as
    /// `leaf`, never over anything there.
    fn seed_into(from: &OwnedFd, parent: &OwnedFd, leaf: &CStr, options: &Options) -> Answer {
        match lstat(parent.as_raw_fd(), leaf) {
            Ok(_) => {
                return (
                    Outcome::Refused,
                    Some("the new worktree already has it; nothing was replaced".into()),
                );
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return (Outcome::Failed, Some(e.to_string())),
        }
        if Instant::now() >= options.deadline {
            return (Outcome::Failed, Some("out of time before it began".into()));
        }
        pause(Phase::Opened);
        let temp = temporary();
        let built = match build(from, parent, &temp, options) {
            Ok(built) => built,
            Err(why) => return (Outcome::Failed, Some(why)),
        };
        pause(Phase::Built);
        match publish(parent.as_raw_fd(), &temp, leaf) {
            Ok(()) => (built.outcome, built.note),
            Err(e) => {
                let removed = remove_tree(parent.as_raw_fd(), &temp, built.id);
                match e.raw_os_error() {
                    Some(libc::EEXIST | libc::ENOTEMPTY) => (
                        Outcome::Refused,
                        Some(format!(
                            "another writer made it while the seed was built; theirs was left \
                             as it was{}",
                            leftover(removed)
                        )),
                    ),
                    Some(libc::EINVAL | libc::ENOSYS | libc::ENOTSUP) => (
                        Outcome::Refused,
                        Some(format!(
                            "this filesystem cannot rename without replacing what is there, so \
                             nothing was seeded{}",
                            leftover(removed)
                        )),
                    ),
                    _ => (Outcome::Failed, Some(format!("{e}{}", leftover(removed)))),
                }
            }
        }
    }

    struct Built {
        outcome: Outcome,
        note: Option<String>,
        /// The temporary folder's identity, so only it is ever removed.
        id: Identity,
    }

    /// What a clone attempt came to. Only macOS clones a whole tree.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    enum Cloning {
        Done(Built),
        /// Not cloned (nothing of it is left); copy instead, saying why
        /// when it was not to be expected.
        Copy(Option<String>),
    }

    /// Make the seed as `temp` in `parent`: a clone where the platform and
    /// filesystem can, else a copy. A failure takes away what it made and
    /// says why.
    fn build(
        from: &OwnedFd,
        parent: &OwnedFd,
        temp: &CStr,
        options: &Options,
    ) -> Result<Built, String> {
        let note = match clone(from, parent, temp, options)? {
            Cloning::Done(built) => return Ok(built),
            Cloning::Copy(note) => note,
        };
        mkdir(parent.as_raw_fd(), temp, 0o700).map_err(|e| e.to_string())?;
        let root = match open_dir(parent.as_raw_fd(), temp) {
            Ok(root) => root,
            Err(e) => {
                // Only an empty folder goes.
                let _ = unlink(parent.as_raw_fd(), temp, true);
                return Err(e.to_string());
            }
        };
        let id = identity(&fstat(root.as_raw_fd()).map_err(|e| e.to_string())?);
        match copy_tree(from, root, options) {
            Ok(copied) => {
                let mut notes: Vec<String> = note.into_iter().collect();
                if copied.left_out > 0 {
                    notes.push(format!("{} special files left out", copied.left_out));
                }
                Ok(Built {
                    outcome: if copied.reflinked {
                        Outcome::Reflinked
                    } else {
                        Outcome::Copied
                    },
                    note: Some(notes.join("; ")).filter(|n| !n.is_empty()),
                    id,
                })
            }
            Err(e) => {
                let removed = remove_tree(parent.as_raw_fd(), temp, id);
                Err(format!("{e}{}", leftover(removed)))
            }
        }
    }

    /// Clone the open directory `from` whole as `temp` in `parent`: one
    /// `fclonefileat(2)`, symlinks inside as symlinks, owned by whoever
    /// runs this.
    #[cfg(target_os = "macos")]
    fn clone(
        from: &OwnedFd,
        parent: &OwnedFd,
        temp: &CStr,
        options: &Options,
    ) -> Result<Cloning, String> {
        // <sys/clonefile.h>; the libc crate has the call but not its flags.
        const CLONE_NOFOLLOW: u32 = 0x0001;
        const CLONE_NOOWNERCOPY: u32 = 0x0002;
        if !options.clone {
            return Ok(Cloning::Copy(None));
        }
        // SAFETY: two open descriptors and a valid C string.
        let cloned = check(unsafe {
            libc::fclonefileat(
                from.as_raw_fd(),
                parent.as_raw_fd(),
                temp.as_ptr(),
                CLONE_NOFOLLOW | CLONE_NOOWNERCOPY,
            )
        });
        match cloned {
            Ok(()) => {
                let id = identity_at(parent.as_raw_fd(), temp)
                    .map_err(|e| format!("cloned, then could not open the clone: {e}"))?;
                Ok(Cloning::Done(Built {
                    outcome: Outcome::Cloned,
                    note: None,
                    id,
                }))
            }
            // The name was somebody's: nothing here made it.
            Err(e) if e.raw_os_error() == Some(libc::EEXIST) => Err(e.to_string()),
            Err(e) => {
                // A clone that failed part way may have left some of itself,
                // under the name only this seed uses.
                if let Ok(id) = identity_at(parent.as_raw_fd(), temp) {
                    let removed = remove_tree(parent.as_raw_fd(), temp, id);
                    if removed.is_err() {
                        return Err(format!("not cloned ({e}){}", leftover(removed)));
                    }
                }
                // A filesystem that cannot clone, or another volume, is
                // expected and not worth a note.
                let expected = matches!(e.raw_os_error(), Some(libc::ENOTSUP | libc::EXDEV));
                Ok(Cloning::Copy(
                    (!expected).then(|| format!("not cloned ({e}); copied")),
                ))
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn clone(
        _from: &OwnedFd,
        _parent: &OwnedFd,
        _temp: &CStr,
        _options: &Options,
    ) -> Result<Cloning, String> {
        Ok(Cloning::Copy(None))
    }

    /// Rename `from` to `to` in `at` unless `to` is there, in one step.
    /// There is no fallback: looking first and renaming after would replace
    /// an empty folder another writer made in between, and holds open, so
    /// a filesystem without such a rename (`EINVAL`, `ENOSYS`, `ENOTSUP`)
    /// gets nothing published.
    fn publish(at: RawFd, from: &CStr, to: &CStr) -> io::Result<()> {
        #[cfg(test)]
        if NO_EXCLUSIVE_RENAME.with(std::cell::Cell::get) {
            return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
        }
        rename_exclusive(at, from, to)
    }

    #[cfg(target_os = "macos")]
    fn rename_exclusive(at: RawFd, from: &CStr, to: &CStr) -> io::Result<()> {
        // SAFETY: valid C strings and an open directory.
        check(unsafe { libc::renameatx_np(at, from.as_ptr(), at, to.as_ptr(), libc::RENAME_EXCL) })
    }

    #[cfg(target_os = "linux")]
    fn rename_exclusive(at: RawFd, from: &CStr, to: &CStr) -> io::Result<()> {
        // The system call itself, so an older C library is no obstacle.
        // SAFETY: valid C strings and an open directory.
        let status = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                at,
                from.as_ptr(),
                at,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn rename_exclusive(_at: RawFd, _from: &CStr, _to: &CStr) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    struct Copied {
        /// There were files, and every one was cloned by reflink.
        reflinked: bool,
        /// Sockets, devices and pipes, which are not copied.
        left_out: usize,
    }

    /// One folder of a copy under way: the source and its copy open, what
    /// is left of its listing, and the permissions it gets once it is full.
    struct Folder {
        from: OwnedFd,
        to: OwnedFd,
        names: std::vec::IntoIter<CString>,
        mode: libc::mode_t,
    }

    /// Copy the open directory `from` into the empty open directory `to`.
    fn copy_tree(from: &OwnedFd, to: OwnedFd, options: &Options) -> io::Result<Copied> {
        let mut reflink = options.clone && cfg!(any(target_os = "linux", target_os = "android"));
        let mut budget = options.budget.unwrap_or_else(|| {
            free_space(to.as_raw_fd()).map_or(UNKNOWN_FREE_BUDGET, |free| free / 2)
        });
        let (mut files, mut left_out) = (0usize, 0usize);
        let mut open = vec![Folder {
            names: list(from)?.into_iter(),
            mode: fstat(from.as_raw_fd())?.st_mode,
            from: from.try_clone()?,
            to,
        }];
        while let Some(folder) = open.last_mut() {
            let (src, dst) = (folder.from.as_raw_fd(), folder.to.as_raw_fd());
            let Some(name) = folder.names.next() else {
                // Everything is in it: it gets the source's permissions.
                let done = open.pop().expect("an open folder");
                // SAFETY: an open directory this copy made.
                check(unsafe { libc::fchmod(done.to.as_raw_fd(), done.mode & 0o7777) })?;
                continue;
            };
            if Instant::now() >= options.deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "out of time"));
            }
            match kind(&lstat(src, &name)?) {
                libc::S_IFDIR => {
                    if open.len() >= MAX_DEPTH {
                        return Err(io::Error::other(format!("deeper than {MAX_DEPTH} folders")));
                    }
                    let from = open_dir(src, &name)?;
                    mkdir(dst, &name, 0o700)?;
                    let to = open_dir(dst, &name)?;
                    open.push(Folder {
                        names: list(&from)?.into_iter(),
                        mode: fstat(from.as_raw_fd())?.st_mode,
                        from,
                        to,
                    });
                }
                libc::S_IFREG => {
                    files += 1;
                    copy_file(src, dst, &name, &mut reflink, &mut budget, options.deadline)?;
                }
                libc::S_IFLNK => copy_link(src, dst, &name)?,
                _ => left_out += 1,
            }
        }
        Ok(Copied {
            reflinked: reflink && files > 0,
            left_out,
        })
    }

    /// Copy the file `name` in `src` to a new one in `dst`, by reflink while
    /// `reflink` holds (the first file that cannot clears it), else in
    /// chunks that stop at `deadline` and before writing more than
    /// `budget`, which is what is left of it afterwards. Permissions and
    /// times are kept.
    fn copy_file(
        src: RawFd,
        dst: RawFd,
        name: &CStr,
        reflink: &mut bool,
        budget: &mut u64,
        deadline: Instant,
    ) -> io::Result<()> {
        // SAFETY: a valid C string and an open directory; a pipe swapped in
        // does not block the open.
        let fd = unsafe {
            libc::openat(
                src,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh descriptor nothing else owns.
        let source = unsafe { File::from_raw_fd(fd) };
        let meta = source.metadata()?;
        if !meta.is_file() {
            return Err(io::Error::other("a file changed while it was copied"));
        }
        // SAFETY: as above; made new, never over anything, owner-only until
        // its bytes are in.
        let fd = unsafe {
            libc::openat(
                dst,
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh descriptor nothing else owns.
        let mut target = unsafe { File::from_raw_fd(fd) };
        if !(*reflink && clone_file(&source, &target).is_ok()) {
            *reflink = false;
            // One byte past the budget is the most a copy writes, however
            // large the file is or however it grows meanwhile.
            loop {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "out of time"));
                }
                let allowed = CHUNK.min(budget.saturating_add(1));
                let written = io::copy(&mut (&source).take(allowed), &mut target)?;
                if written > *budget {
                    return Err(io::Error::new(
                        io::ErrorKind::StorageFull,
                        "a plain copy would take more than half the free space",
                    ));
                }
                *budget -= written;
                if written < allowed {
                    break;
                }
            }
        }
        target.set_permissions(meta.permissions())?;
        filetime::set_file_handle_times(
            &target,
            Some(filetime::FileTime::from_last_access_time(&meta)),
            Some(filetime::FileTime::from_last_modification_time(&meta)),
        )
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn clone_file(source: &File, target: &File) -> io::Result<()> {
        // SAFETY: both descriptors are open for the length of the call.
        if unsafe { libc::ioctl(target.as_raw_fd(), libc::FICLONE, source.as_raw_fd()) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn clone_file(_source: &File, _target: &File) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    /// Make the symlink `name` in `src` again in `dst`, pointing where it
    /// points.
    fn copy_link(src: RawFd, dst: RawFd, name: &CStr) -> io::Result<()> {
        let mut target = vec![0u8; libc::PATH_MAX as usize + 1];
        // SAFETY: a valid C string, an open directory and a buffer of the
        // length given.
        let length = unsafe {
            libc::readlinkat(src, name.as_ptr(), target.as_mut_ptr().cast(), target.len())
        };
        if length < 0 {
            return Err(io::Error::last_os_error());
        }
        let length = length as usize;
        if length >= target.len() {
            return Err(io::Error::other("a symlink too long to copy"));
        }
        target.truncate(length);
        let target = cstr(&target)?;
        // SAFETY: valid C strings and an open directory.
        check(unsafe { libc::symlinkat(target.as_ptr(), dst, name.as_ptr()) })
    }

    /// The bytes an unprivileged writer may still use on the filesystem
    /// the open `at` is on.
    // The field types differ by platform; every one fits in a u64.
    #[allow(clippy::unnecessary_cast)]
    fn free_space(at: RawFd) -> Option<u64> {
        let mut stat = MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: an open descriptor and a buffer of the size the call fills.
        if unsafe { libc::fstatvfs(at, stat.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: filled by the successful call above.
        let stat = unsafe { stat.assume_init() };
        Some((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::unix::{NO_EXCLUSIVE_RENAME, PAUSE, Phase};
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    const LIMIT: Duration = Duration::from_secs(60);

    fn options(clone: bool, budget: Option<u64>) -> Options {
        Options {
            deadline: Instant::now() + LIMIT,
            clone,
            budget,
        }
    }

    /// `seed_with`, with `hook` called at each phase.
    fn seed_pausing(
        source: &Path,
        worktree: &Path,
        names: &[&str],
        options: &Options,
        hook: impl FnMut(Phase) + 'static,
    ) -> Vec<Seeded> {
        PAUSE.with(|pause| *pause.borrow_mut() = Some(Box::new(hook)));
        let names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
        let seeded = seed_with(source, worktree, &names, options);
        PAUSE.with(|pause| *pause.borrow_mut() = None);
        seeded
    }

    /// The tree under `root`, as relative path → what is there: a folder
    /// and its mode, a file's bytes, mode and modification time, or a
    /// symlink's target.
    fn snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, String> {
        use std::os::unix::fs::PermissionsExt;
        let mut out = std::collections::BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let meta = fs::symlink_metadata(&path).unwrap();
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                let mode = meta.permissions().mode() & 0o7777;
                let what = if meta.is_dir() {
                    pending.push(path.clone());
                    format!("folder mode={mode:o}")
                } else if meta.file_type().is_symlink() {
                    format!("link to {}", fs::read_link(&path).unwrap().display())
                } else {
                    format!(
                        "{:?} mode={mode:o} modified={:?}",
                        fs::read(&path).unwrap(),
                        filetime::FileTime::from_last_modification_time(&meta),
                    )
                };
                out.insert(relative, what);
            }
        }
        out
    }

    /// A build directory at `at` with nested folders, an empty one, an
    /// executable, an old modification time and a relative symlink.
    fn build_output(at: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(at.join("debug/deps")).unwrap();
        fs::create_dir_all(at.join("debug/incremental/empty")).unwrap();
        fs::write(
            at.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();
        fs::write(at.join("debug/deps/libx.rlib"), vec![7u8; 70_000]).unwrap();
        fs::write(at.join("debug/app"), b"\x7fELF binary").unwrap();
        let old = filetime::FileTime::from_unix_time(1_600_000_000, 0);
        filetime::set_file_mtime(at.join("debug/deps/libx.rlib"), old).unwrap();
        fs::set_permissions(at.join("debug/app"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("deps/libx.rlib", at.join("debug/libx.rlib")).unwrap();
    }

    fn checkouts() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let (source, worktree) = (tmp.path().join("source"), tmp.path().join("worktree"));
        fs::create_dir(&source).unwrap();
        fs::create_dir(&worktree).unwrap();
        (tmp, source, worktree)
    }

    /// What is in a folder, by name, in order.
    fn names(at: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(at)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_seeded_directory_has_the_source_contents_and_a_missing_one_is_skipped() {
        for clone in [true, false] {
            let (_tmp, source, worktree) = checkouts();
            build_output(&source.join("target"));
            let seeded = seed_pausing(
                &source,
                &worktree,
                &["target", "node_modules"],
                &options(clone, None),
                |_| {},
            );
            assert_eq!(seeded.len(), 2);
            assert!(seeded[0].outcome.seeded(), "{seeded:?}");
            if !clone {
                assert_eq!(seeded[0].outcome, Outcome::Copied, "{seeded:?}");
            }
            assert_eq!(
                snapshot(&source.join("target")),
                snapshot(&worktree.join("target"))
            );
            assert_eq!(
                (seeded[1].dir.as_str(), seeded[1].outcome),
                ("node_modules", Outcome::Skipped)
            );
            // Nothing else is left in the new worktree: no temporary name.
            assert_eq!(names(&worktree), ["target"]);
        }
    }

    /// A read-only folder is copied read-only, once what is in it is in.
    #[test]
    fn a_read_only_folder_is_copied_with_its_contents() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, source, worktree) = checkouts();
        build_output(&source.join("target"));
        let sealed = source.join("target/debug/deps");
        fs::set_permissions(&sealed, fs::Permissions::from_mode(0o555)).unwrap();
        let seeded = seed_with(
            &source,
            &worktree,
            &["target".into()],
            &options(false, None),
        );
        let same = snapshot(&source.join("target")) == snapshot(&worktree.join("target"));
        for at in [&sealed, &worktree.join("target/debug/deps")] {
            let _ = fs::set_permissions(at, fs::Permissions::from_mode(0o755));
        }
        assert_eq!(seeded[0].outcome, Outcome::Copied, "{seeded:?}");
        assert!(same);
    }

    /// On macOS the whole directory is cloned when the filesystem is APFS;
    /// anywhere else the test has nothing to show.
    #[cfg(target_os = "macos")]
    #[test]
    fn on_apfs_the_directory_is_cloned() {
        let (_tmp, source, worktree) = checkouts();
        if !apfs(&source) {
            eprintln!("skipped: the temporary directory is not on APFS");
            return;
        }
        build_output(&source.join("target"));
        let seeded = seed(&source, &worktree, &["target".into()], LIMIT);
        assert_eq!(seeded[0].outcome, Outcome::Cloned, "{seeded:?}");
        assert_eq!(seeded[0].note, None);
        assert_eq!(
            snapshot(&source.join("target")),
            snapshot(&worktree.join("target"))
        );
        // A clone is its own copy: a change on one side stays there.
        fs::write(worktree.join("target/CACHEDIR.TAG"), "changed").unwrap();
        assert_ne!(
            fs::read(source.join("target/CACHEDIR.TAG")).unwrap(),
            b"changed"
        );
    }

    #[cfg(target_os = "macos")]
    fn apfs(at: &Path) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(at.as_os_str().as_bytes()).unwrap();
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: a valid C string and a buffer of the size the call fills.
        if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return false;
        }
        // SAFETY: filled by the successful call above.
        let stat = unsafe { stat.assume_init() };
        // SAFETY: the kernel NUL-terminates the type name.
        unsafe { std::ffi::CStr::from_ptr(stat.f_fstypename.as_ptr()) }.to_bytes() == b"apfs"
    }

    /// No symlink on the way to a named directory is followed, wherever it
    /// leads, and a file is not a directory.
    #[test]
    fn a_symlink_on_the_way_in_the_source_is_refused_wherever_it_leads() {
        let (tmp, source, worktree) = checkouts();
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret"), "x").unwrap();
        build_output(&source.join("target"));
        std::os::unix::fs::symlink(&outside, source.join("escape")).unwrap();
        std::os::unix::fs::symlink("target", source.join("inside")).unwrap();
        std::os::unix::fs::symlink("nowhere", source.join("dangling")).unwrap();
        fs::create_dir(source.join("out")).unwrap();
        std::os::unix::fs::symlink("../target", source.join("out/target")).unwrap();
        std::os::unix::fs::symlink(".", source.join("itself")).unwrap();
        fs::write(source.join("file"), "x").unwrap();
        let asked = [
            "escape",
            "inside",
            "dangling",
            "out/target",
            "itself/target",
            "file",
        ];
        let seeded = seed(&source, &worktree, &asked.map(str::to_owned), LIMIT);
        for refused in &seeded {
            assert_eq!(refused.outcome, Outcome::Refused, "{refused:?}");
        }
        assert!(seeded[0].note.as_deref().unwrap().contains("symlink"));
        assert!(
            seeded[5]
                .note
                .as_deref()
                .unwrap()
                .contains("not a directory")
        );
        assert!(names(&worktree).is_empty(), "{:?}", names(&worktree));
    }

    #[test]
    fn names_that_are_not_plain_relative_directories_are_refused() {
        let (_tmp, source, worktree) = checkouts();
        build_output(&source.join("target"));
        fs::create_dir_all(source.join(".git/objects")).unwrap();
        let asked = [
            "/target",
            "../source/target",
            "target/../target",
            ".git",
            "",
        ];
        let seeded = seed(&source, &worktree, &asked.map(str::to_owned), LIMIT);
        assert!(
            seeded.iter().all(|s| s.outcome == Outcome::Refused),
            "{seeded:?}"
        );
        assert!(names(&worktree).is_empty());
    }

    /// The new checkout keeps what it has, and nothing goes through a
    /// symlink or a file it carries.
    #[test]
    fn nothing_in_the_worktree_is_replaced_or_written_through() {
        let (tmp, source, worktree) = checkouts();
        build_output(&source.join("target"));
        fs::create_dir_all(source.join("web/node_modules/pkg")).unwrap();
        fs::create_dir_all(source.join("app/node_modules/pkg")).unwrap();
        fs::create_dir(worktree.join("target")).unwrap();
        fs::write(worktree.join("target/tracked"), "kept").unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, worktree.join("web")).unwrap();
        fs::write(worktree.join("app"), "a file").unwrap();
        let seeded = seed(
            &source,
            &worktree,
            &[
                "target".into(),
                "web/node_modules".into(),
                "app/node_modules".into(),
            ],
            LIMIT,
        );
        assert!(
            seeded.iter().all(|s| s.outcome == Outcome::Refused),
            "{seeded:?}"
        );
        assert_eq!(names(&worktree.join("target")), ["tracked"]);
        assert!(names(&elsewhere).is_empty());
        assert_eq!(fs::read_to_string(worktree.join("app")).unwrap(), "a file");
        assert_eq!(names(&worktree), ["app", "target", "web"]);
    }

    /// Between the checks and the writes, a folder on the way in the new
    /// worktree is moved aside and a symlink to somewhere else put in its
    /// place, and the same is done in the source: the seed still reads the
    /// source that was checked and writes into the folder that was
    /// checked, and nothing reaches the other place.
    #[test]
    fn an_ancestor_swapped_for_a_symlink_mid_seed_redirects_nothing() {
        for clone in [true, false] {
            let (tmp, source, worktree) = checkouts();
            build_output(&source.join("web/node_modules"));
            fs::create_dir(worktree.join("web")).unwrap();
            let outside = tmp.path().join("outside");
            fs::create_dir_all(outside.join("node_modules")).unwrap();
            fs::write(outside.join("node_modules/planted"), "not the source").unwrap();
            let (wt, src, out) = (worktree.clone(), source.clone(), outside.clone());
            let seeded = seed_pausing(
                &source,
                &worktree,
                &["web/node_modules"],
                &options(clone, None),
                move |phase| {
                    if phase == Phase::Opened {
                        fs::rename(wt.join("web"), wt.join("web-moved")).unwrap();
                        std::os::unix::fs::symlink(&out, wt.join("web")).unwrap();
                        fs::rename(src.join("web"), src.join("web-moved")).unwrap();
                        std::os::unix::fs::symlink(&out, src.join("web")).unwrap();
                    }
                },
            );
            assert!(seeded[0].outcome.seeded(), "{seeded:?}");
            assert_eq!(names(&outside), ["node_modules"], "nothing written outside");
            assert_eq!(names(&outside.join("node_modules")), ["planted"]);
            assert_eq!(
                snapshot(&source.join("web-moved/node_modules")),
                snapshot(&worktree.join("web-moved/node_modules")),
                "the checked source, in the checked folder"
            );
            assert_eq!(names(&worktree.join("web-moved")), ["node_modules"]);
        }
    }

    /// Another writer makes the destination after the seed found it free:
    /// theirs stays as it is, empty or not, and the seed is removed.
    #[test]
    fn a_destination_made_by_someone_else_mid_seed_is_left_intact() {
        for (clone, with_a_file) in [(true, false), (true, true), (false, false), (false, true)] {
            let (_tmp, source, worktree) = checkouts();
            build_output(&source.join("target"));
            let wt = worktree.clone();
            let seeded = seed_pausing(
                &source,
                &worktree,
                &["target"],
                &options(clone, None),
                move |phase| {
                    if phase == Phase::Built {
                        fs::create_dir(wt.join("target")).unwrap();
                        if with_a_file {
                            fs::write(wt.join("target/theirs"), "theirs").unwrap();
                        }
                    }
                },
            );
            assert_eq!(seeded[0].outcome, Outcome::Refused, "{seeded:?}");
            assert!(
                seeded[0]
                    .note
                    .as_deref()
                    .unwrap()
                    .contains("another writer"),
                "{seeded:?}"
            );
            let theirs: &[&str] = if with_a_file { &["theirs"] } else { &[] };
            assert_eq!(names(&worktree.join("target")), theirs);
            assert_eq!(names(&worktree), ["target"], "the seed was removed");
        }
    }

    /// Where the filesystem has no rename that refuses to replace, nothing
    /// is published: not even when the name is free when the seed looks,
    /// since an empty folder another writer makes and holds open in the
    /// moment after would be replaced.
    #[test]
    fn without_an_exclusive_rename_nothing_is_published() {
        use std::os::unix::fs::MetadataExt;
        for (clone, someone_makes_it) in
            [(true, false), (true, true), (false, false), (false, true)]
        {
            let (_tmp, source, worktree) = checkouts();
            build_output(&source.join("target"));
            let wt = worktree.clone();
            let theirs = std::rc::Rc::new(std::cell::Cell::new(None));
            let made = theirs.clone();
            NO_EXCLUSIVE_RENAME.with(|flag| flag.set(true));
            let seeded = seed_pausing(
                &source,
                &worktree,
                &["target"],
                &options(clone, None),
                move |phase| {
                    if phase == Phase::Built && someone_makes_it {
                        fs::create_dir(wt.join("target")).unwrap();
                        made.set(Some(fs::metadata(wt.join("target")).unwrap().ino()));
                    }
                },
            );
            NO_EXCLUSIVE_RENAME.with(|flag| flag.set(false));
            assert_eq!(seeded[0].outcome, Outcome::Refused, "{seeded:?}");
            assert!(
                seeded[0]
                    .note
                    .as_deref()
                    .unwrap()
                    .contains("without replacing"),
                "{seeded:?}"
            );
            match theirs.get() {
                Some(inode) => {
                    // Theirs is the same folder it was, and still empty.
                    let now = fs::metadata(worktree.join("target")).unwrap();
                    assert_eq!(now.ino(), inode);
                    assert!(names(&worktree.join("target")).is_empty());
                    assert_eq!(names(&worktree), ["target"]);
                }
                None => assert!(names(&worktree).is_empty(), "{:?}", names(&worktree)),
            }
        }
    }

    /// A file larger than the budget stops the copy part way through it,
    /// and what was copied is removed, folders made on the way included.
    #[test]
    fn a_file_larger_than_the_budget_stops_the_copy_and_leaves_nothing() {
        let (_tmp, source, worktree) = checkouts();
        let at = source.join("cache/big");
        fs::create_dir_all(&at).unwrap();
        fs::write(at.join("small"), "small").unwrap();
        fs::write(at.join("blob"), vec![1u8; 4 * 1024 * 1024]).unwrap();
        let seeded = seed_with(
            &source,
            &worktree,
            &["cache/big".into()],
            &options(false, Some(1024 * 1024)),
        );
        assert_eq!(seeded[0].outcome, Outcome::Failed, "{seeded:?}");
        assert!(
            seeded[0].note.as_deref().unwrap().contains("free space"),
            "{seeded:?}"
        );
        assert!(names(&worktree).is_empty(), "{:?}", names(&worktree));
    }

    /// Out of time before it begins, or part way through a copy: failed,
    /// and nothing is left.
    #[test]
    fn a_seed_out_of_time_fails_and_leaves_nothing() {
        let (_tmp, source, worktree) = checkouts();
        build_output(&source.join("target"));
        let seeded = seed(&source, &worktree, &["target".into()], Duration::ZERO);
        assert_eq!(seeded[0].outcome, Outcome::Failed);
        assert!(names(&worktree).is_empty());
        let soon = Options {
            deadline: Instant::now() + Duration::from_millis(200),
            clone: false,
            budget: None,
        };
        let seeded = seed_pausing(&source, &worktree, &["target"], &soon, |phase| {
            if phase == Phase::Opened {
                std::thread::sleep(Duration::from_millis(300));
            }
        });
        assert_eq!(seeded[0].outcome, Outcome::Failed, "{seeded:?}");
        assert!(
            seeded[0].note.as_deref().unwrap().contains("out of time"),
            "{seeded:?}"
        );
        assert!(names(&worktree).is_empty(), "{:?}", names(&worktree));
    }

    #[test]
    fn missing_checkouts_fail_every_seed_without_panicking() {
        let tmp = tempfile::tempdir().unwrap();
        let seeded = seed(
            &tmp.path().join("gone"),
            tmp.path(),
            &["target".into()],
            LIMIT,
        );
        assert_eq!(seeded[0].outcome, Outcome::Failed);
    }
}

#[cfg(all(test, not(unix)))]
mod tests {
    use super::*;

    #[test]
    fn this_platform_seeds_nothing_and_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("source/target")).unwrap();
        std::fs::create_dir(tmp.path().join("worktree")).unwrap();
        let seeded = seed(
            &tmp.path().join("source"),
            &tmp.path().join("worktree"),
            &["target".into()],
            Duration::from_secs(5),
        );
        assert_eq!(seeded[0].outcome, Outcome::Refused);
        assert!(seeded[0].note.as_deref().unwrap().contains("platform"));
        assert!(!tmp.path().join("worktree/target").exists());
    }
}
