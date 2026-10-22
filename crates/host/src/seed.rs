//! Seeding a new worktree with the build output of the checkout it was made
//! from (`worktree_create`'s `seed_build`), so its first build starts warm.
//!
//! Each named directory is cloned whole by `clonefile(2)` on macOS: on APFS
//! that is copy-on-write, near-instant, and takes no space until one side
//! changes a file. Where that cannot be done (another filesystem, another
//! volume, another platform) it is copied file by file instead, each file
//! by reflink where Linux's filesystem shares extents (`FICLONE`) and byte
//! for byte otherwise. Permissions and times are kept either way, so a
//! build tool that compares times rebuilds what the new checkout changed
//! rather than trusting output newer than its sources. Symlinks inside are
//! copied as symlinks, never followed.
//!
//! Nothing here fails the worktree: each directory gets an [`Outcome`], and
//! a copy that fails or runs out of time is removed rather than left half
//! made. A clone is one system call, which is not interrupted; the time
//! limit bounds the copies, checked between files.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use agentdocker_core::seed::{self, Outcome, Seeded};

/// Seed `worktree` with each of `names` from `source`, all within `limit`.
/// Both are checkout roots; the names are checked again here, so a caller
/// that skipped [`seed::check`] still gets nothing outside them.
pub fn seed(source: &Path, worktree: &Path, names: &[String], limit: Duration) -> Vec<Seeded> {
    let deadline = Instant::now() + limit;
    let roots = fs::canonicalize(source).and_then(|s| Ok((s, fs::canonicalize(worktree)?)));
    names
        .iter()
        .map(|name| {
            let (outcome, note) = match &roots {
                Ok((source, worktree)) => one(source, worktree, name, deadline),
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

fn one(source: &Path, worktree: &Path, name: &str, deadline: Instant) -> (Outcome, Option<String>) {
    let refused = |why: &str| (Outcome::Refused, Some(why.to_owned()));
    let relative = match seed::relative(Path::new(name)) {
        Ok(relative) => relative,
        Err(why) => return refused(why),
    };
    let named = source.join(&relative);
    match fs::symlink_metadata(&named) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return (
                Outcome::Skipped,
                Some("the source checkout has no such directory".into()),
            );
        }
        Err(e) => return (Outcome::Failed, Some(e.to_string())),
    }
    // A symlink on the way may lead anywhere: what the name resolves to
    // must still be a directory inside the checkout, other than the
    // checkout itself or git's own.
    let from = match fs::canonicalize(&named) {
        Ok(from) => from,
        Err(_) => return refused("a symlink that leads nowhere"),
    };
    let Ok(inside) = from.strip_prefix(source) else {
        return refused("leads outside the source checkout");
    };
    if seed::relative(inside).is_err() {
        return refused("leads to the checkout itself or to git's own directory");
    }
    if !from.is_dir() {
        return refused("not a directory");
    }
    if !clear_way(worktree, &relative) {
        return refused("the new worktree has a symlink on the way");
    }
    let to = worktree.join(&relative);
    if fs::symlink_metadata(&to).is_ok() {
        return refused("the new worktree already has it; nothing was replaced");
    }
    if let Err(e) = to.parent().map_or(Ok(()), fs::create_dir_all) {
        return (Outcome::Failed, Some(e.to_string()));
    }
    if Instant::now() >= deadline {
        return (Outcome::Failed, Some("out of time before it began".into()));
    }
    let not_cloned = match clone_tree(&from, &to) {
        Ok(()) => return (Outcome::Cloned, None),
        Err(e) => {
            // A clone that failed part way may have left some of itself.
            if fs::symlink_metadata(&to).is_ok() {
                let _ = fs::remove_dir_all(&to);
            }
            (e.kind() != io::ErrorKind::Unsupported).then(|| format!("not cloned ({e}); copied"))
        }
    };
    match copy_tree(&from, &to, deadline) {
        Ok(copy) => {
            let mut notes: Vec<String> = not_cloned.into_iter().collect();
            if copy.left_out > 0 {
                notes.push(format!("{} special files or links left out", copy.left_out));
            }
            let outcome = if copy.reflinked {
                Outcome::Reflinked
            } else {
                Outcome::Copied
            };
            (outcome, Some(notes.join("; ")).filter(|n| !n.is_empty()))
        }
        Err(e) => {
            let removed = fs::remove_dir_all(&to).is_ok() || fs::symlink_metadata(&to).is_err();
            let left = if removed {
                "nothing was left behind"
            } else {
                "some of the copy could not be removed"
            };
            (Outcome::Failed, Some(format!("{e}; {left}")))
        }
    }
}

/// Whether `relative` can be made beneath `base` without going through a
/// symlink: a checkout can carry one where a folder on the way goes, and
/// written through, the seed would land wherever it points.
fn clear_way(base: &Path, relative: &Path) -> bool {
    let mut at = base.to_path_buf();
    relative.components().all(|part| {
        at.push(part);
        !fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink())
    })
}

/// Clone the directory `from` as a new `to` with one `clonefile(2)`: the
/// whole tree, symlinks as symlinks, owned by whoever runs this.
#[cfg(target_os = "macos")]
fn clone_tree(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    // <sys/clonefile.h>; the libc crate has the call but not its flags.
    const CLONE_NOFOLLOW: u32 = 0x0001;
    const CLONE_NOOWNERCOPY: u32 = 0x0002;
    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: two valid C strings that outlive the call.
    let status = unsafe {
        libc::clonefile(
            from.as_ptr(),
            to.as_ptr(),
            CLONE_NOFOLLOW | CLONE_NOOWNERCOPY,
        )
    };
    if status == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    // The filesystem cannot clone, or the two are on different volumes:
    // expected, and said as `Unsupported` so no note is made of it.
    match error.raw_os_error() {
        Some(libc::ENOTSUP | libc::EXDEV) => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this filesystem cannot clone",
        )),
        _ => Err(error),
    }
}

#[cfg(not(target_os = "macos"))]
fn clone_tree(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no whole-tree clone on this platform",
    ))
}

struct Copied {
    /// There were files, and every one was cloned by reflink.
    reflinked: bool,
    /// Sockets, devices and, where they cannot be made, symlinks.
    left_out: usize,
}

/// Copy the directory `from` as a new `to`, file by file, until `deadline`.
/// A plain copy stops once it has written half the space that was free when
/// it began. On an error the caller removes `to`.
fn copy_tree(from: &Path, to: &Path, deadline: Instant) -> io::Result<Copied> {
    let out_of_time = || io::Error::new(io::ErrorKind::TimedOut, "out of time");
    let mut reflink = cfg!(any(target_os = "linux", target_os = "android"));
    let budget = to.parent().and_then(free_space).map(|free| free / 2);
    let (mut written, mut files, mut left_out) = (0u64, 0usize, 0usize);
    // Folders get their own permissions once everything is in them, in
    // case one is read-only.
    let mut folders = Vec::new();
    let mut pending = vec![(from.to_path_buf(), to.to_path_buf())];
    while let Some((from, to)) = pending.pop() {
        fs::create_dir(&to)?;
        folders.push((to.clone(), fs::symlink_metadata(&from)?.permissions()));
        for entry in fs::read_dir(&from)? {
            if Instant::now() >= deadline {
                return Err(out_of_time());
            }
            let entry = entry?;
            let (source, target) = (entry.path(), to.join(entry.file_name()));
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push((source, target));
            } else if kind.is_file() {
                files += 1;
                written += copy_file(&source, &target, &mut reflink)?;
                if budget.is_some_and(|budget| written > budget) {
                    return Err(io::Error::new(
                        io::ErrorKind::StorageFull,
                        "a plain copy would take more than half the free space",
                    ));
                }
            } else if !copy_symlink(&source, &target, kind)? {
                left_out += 1;
            }
        }
    }
    for (folder, permissions) in folders.into_iter().rev() {
        fs::set_permissions(folder, permissions)?;
    }
    Ok(Copied {
        reflinked: reflink && files > 0,
        left_out,
    })
}

/// Copy one file to a new `to`, by reflink while `reflink` holds (the first
/// file that cannot be clears it, and the rest are copied), keeping its
/// permissions and times. Returns the bytes written: none for a reflink.
fn copy_file(from: &Path, to: &Path, reflink: &mut bool) -> io::Result<u64> {
    let mut source = File::open(from)?;
    let meta = source.metadata()?;
    let mut target = OpenOptions::new().write(true).create_new(true).open(to)?;
    let mut written = 0;
    if !(*reflink && clone_file(&source, &target).is_ok()) {
        *reflink = false;
        written = io::copy(&mut source, &mut target)?;
    }
    target.set_permissions(meta.permissions())?;
    filetime::set_file_handle_times(
        &target,
        Some(filetime::FileTime::from_last_access_time(&meta)),
        Some(filetime::FileTime::from_last_modification_time(&meta)),
    )?;
    Ok(written)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn clone_file(source: &File, target: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
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

/// Make the symlink `from` again at `to`, pointing where it points. Says
/// whether it was made; anything else that is neither file nor folder is
/// left out.
#[cfg(unix)]
fn copy_symlink(from: &Path, to: &Path, kind: fs::FileType) -> io::Result<bool> {
    if !kind.is_symlink() {
        return Ok(false);
    }
    std::os::unix::fs::symlink(fs::read_link(from)?, to)?;
    Ok(true)
}

#[cfg(not(unix))]
fn copy_symlink(_from: &Path, _to: &Path, _kind: fs::FileType) -> io::Result<bool> {
    // Making a symlink on Windows takes a privilege a daemon may not have.
    Ok(false)
}

/// The bytes an unprivileged writer may still use on the filesystem `at`
/// is on, where the platform says.
#[cfg(unix)]
// The field types differ by platform; every one fits in a u64.
#[allow(clippy::unnecessary_cast)]
fn free_space(at: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(at.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: a valid C string and a buffer of the size the call fills.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: filled by the successful call above.
    let stat = unsafe { stat.assume_init() };
    Some((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

#[cfg(not(unix))]
fn free_space(_at: &Path) -> Option<u64> {
    None
}

/// The tree under `root`, as relative path → what is there: a folder, a
/// file's bytes, mode and modification time, or a symlink's target.
#[cfg(test)]
fn snapshot(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, String> {
    let mut out = std::collections::BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            let what = if meta.is_dir() {
                pending.push(path.clone());
                "folder".to_owned()
            } else if meta.file_type().is_symlink() {
                format!("link to {}", fs::read_link(&path).unwrap().display())
            } else {
                format!(
                    "{:?} readonly={} modified={:?}",
                    fs::read(&path).unwrap(),
                    meta.permissions().readonly(),
                    filetime::FileTime::from_last_modification_time(&meta),
                ) + &mode(&meta)
            };
            out.insert(relative, what);
        }
    }
    out
}

#[cfg(all(test, unix))]
fn mode(meta: &fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    format!(" mode={:o}", meta.permissions().mode() & 0o7777)
}

#[cfg(all(test, not(unix)))]
fn mode(_meta: &fs::Metadata) -> String {
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: Duration = Duration::from_secs(60);

    /// A build directory with nested folders, an empty one, an executable,
    /// an old modification time and (on Unix) a relative symlink.
    fn build_output(root: &Path) {
        let target = root.join("target");
        fs::create_dir_all(target.join("debug/deps")).unwrap();
        fs::create_dir_all(target.join("debug/incremental/empty")).unwrap();
        fs::write(
            target.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();
        fs::write(target.join("debug/deps/libx.rlib"), vec![7u8; 70_000]).unwrap();
        fs::write(target.join("debug/app"), b"\x7fELF binary").unwrap();
        let old = filetime::FileTime::from_unix_time(1_600_000_000, 0);
        filetime::set_file_mtime(target.join("debug/deps/libx.rlib"), old).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(target.join("debug/app"), fs::Permissions::from_mode(0o755))
                .unwrap();
            std::os::unix::fs::symlink("deps/libx.rlib", target.join("debug/libx.rlib")).unwrap();
        }
    }

    fn checkouts() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let (source, worktree) = (tmp.path().join("source"), tmp.path().join("worktree"));
        fs::create_dir(&source).unwrap();
        fs::create_dir(&worktree).unwrap();
        (tmp, source, worktree)
    }

    #[test]
    fn a_seeded_directory_has_the_source_contents_and_a_missing_one_is_skipped() {
        let (_tmp, source, worktree) = checkouts();
        build_output(&source);
        let seeded = seed(
            &source,
            &worktree,
            &["target".into(), "node_modules".into()],
            LIMIT,
        );
        assert_eq!(seeded.len(), 2);
        assert!(seeded[0].outcome.seeded(), "{seeded:?}");
        assert_eq!(
            snapshot(&source.join("target")),
            snapshot(&worktree.join("target"))
        );
        assert_eq!(
            (seeded[1].dir.as_str(), seeded[1].outcome),
            ("node_modules", Outcome::Skipped)
        );
        assert!(!worktree.join("node_modules").exists());
    }

    /// The file-by-file copy every platform falls back to keeps contents,
    /// permissions, times and symlinks.
    #[test]
    fn the_fallback_copy_keeps_contents_permissions_times_and_links() {
        let (_tmp, source, worktree) = checkouts();
        build_output(&source);
        let copy = copy_tree(
            &source.join("target"),
            &worktree.join("target"),
            Instant::now() + LIMIT,
        )
        .unwrap();
        assert_eq!(copy.left_out, 0);
        assert_eq!(
            snapshot(&source.join("target")),
            snapshot(&worktree.join("target"))
        );
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
        build_output(&source);
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

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_checkout_is_refused_and_one_inside_is_followed() {
        let (tmp, source, worktree) = checkouts();
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret"), "x").unwrap();
        std::os::unix::fs::symlink(&outside, source.join("escape")).unwrap();
        std::os::unix::fs::symlink("../outside", source.join("relative-escape")).unwrap();
        std::os::unix::fs::symlink(".", source.join("itself")).unwrap();
        std::os::unix::fs::symlink("nowhere", source.join("dangling")).unwrap();
        fs::create_dir_all(source.join(".git/objects")).unwrap();
        std::os::unix::fs::symlink(".git", source.join("gitdir")).unwrap();
        build_output(&source);
        fs::create_dir(source.join("out")).unwrap();
        std::os::unix::fs::symlink("../target", source.join("out/target")).unwrap();
        let seeded = seed(
            &source,
            &worktree,
            &[
                "escape".into(),
                "relative-escape".into(),
                "itself".into(),
                "dangling".into(),
                "gitdir".into(),
                "out/target".into(),
            ],
            LIMIT,
        );
        for refused in &seeded[..5] {
            assert_eq!(refused.outcome, Outcome::Refused, "{refused:?}");
            assert!(!worktree.join(&refused.dir).exists());
        }
        assert!(seeded[0].note.as_deref().unwrap().contains("outside"));
        // Inside the checkout: the folder it leads to is seeded under the
        // name it was asked for.
        assert!(seeded[5].outcome.seeded(), "{:?}", seeded[5]);
        assert_eq!(
            snapshot(&source.join("target")),
            snapshot(&worktree.join("out/target"))
        );
    }

    #[test]
    fn names_that_are_not_plain_relative_directories_are_refused() {
        let (_tmp, source, worktree) = checkouts();
        build_output(&source);
        fs::write(source.join("file"), "x").unwrap();
        let names = [
            "/target",
            "../source/target",
            "target/../target",
            ".git",
            "",
            "file",
        ];
        let seeded = seed(&source, &worktree, &names.map(str::to_owned), LIMIT);
        assert!(
            seeded.iter().all(|s| s.outcome == Outcome::Refused),
            "{seeded:?}"
        );
        assert_eq!(fs::read_dir(&worktree).unwrap().count(), 0);
    }

    /// The new checkout keeps what it has, and nothing goes through a
    /// symlink it carries.
    #[test]
    fn nothing_in_the_worktree_is_replaced_or_written_through() {
        let (tmp, source, worktree) = checkouts();
        build_output(&source);
        fs::create_dir_all(source.join("web/node_modules/pkg")).unwrap();
        fs::create_dir(worktree.join("target")).unwrap();
        fs::write(worktree.join("target/tracked"), "kept").unwrap();
        let seeded = seed(&source, &worktree, &["target".into()], LIMIT);
        assert_eq!(seeded[0].outcome, Outcome::Refused);
        assert_eq!(
            fs::read_dir(worktree.join("target")).unwrap().count(),
            1,
            "nothing added beside what was there"
        );
        #[cfg(unix)]
        {
            let elsewhere = tmp.path().join("elsewhere");
            fs::create_dir(&elsewhere).unwrap();
            std::os::unix::fs::symlink(&elsewhere, worktree.join("web")).unwrap();
            let seeded = seed(&source, &worktree, &["web/node_modules".into()], LIMIT);
            assert_eq!(seeded[0].outcome, Outcome::Refused, "{seeded:?}");
            assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
        }
        let _ = tmp;
    }

    #[test]
    fn a_seed_out_of_time_fails_and_leaves_nothing() {
        let (_tmp, source, worktree) = checkouts();
        build_output(&source);
        let seeded = seed(&source, &worktree, &["target".into()], Duration::ZERO);
        assert_eq!(seeded[0].outcome, Outcome::Failed);
        assert!(!worktree.join("target").exists());
        // A copy that runs out part way says so, and the caller removes it.
        let error = copy_tree(
            &source.join("target"),
            &worktree.join("target"),
            Instant::now(),
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
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
