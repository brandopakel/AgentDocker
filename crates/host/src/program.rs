//! One executable, two programs. The `agentdocker` file is the CLI, the MCP
//! server, the hooks adapter and the daemon at once; which one a process
//! runs is the name it was invoked by (its `argv[0]`'s file name): the
//! daemon when that is `agentd` (`agentd.exe`, in any case, on Windows),
//! the CLI otherwise. Packages ship `agentd` beside `agentdocker` as a hard
//! link to it, so the daemon's code is installed once and every `agentd …`
//! that worked before still does; `agentdocker daemon run …` runs the
//! daemon under the CLI's own name, where no link exists.
//!
//! What the kernel says a process runs is a file, not a name: on macOS two
//! processes of one hard-linked file can both be reported under whichever
//! of its names was looked up last. So nothing may tell the daemon from the
//! CLI by the kernel's spelling alone. [`spelled_as`] puts a kernel-reported
//! path into the name the process was invoked by, and only between the two
//! names of one file.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The CLI's name: what the executable is built as.
pub const CLI: &str = "agentdocker";
/// The daemon's name: a link to the CLI's file.
pub const DAEMON: &str = "agentd";

/// Which program a process of the executable runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Cli,
    Daemon,
}

impl Role {
    /// The program's name, without a platform suffix.
    pub fn name(self) -> &'static str {
        match self {
            Role::Cli => CLI,
            Role::Daemon => DAEMON,
        }
    }

    /// The file name that runs this program here: `agentd`, or
    /// `agentd.exe` on Windows.
    pub fn file_name(self) -> String {
        format!("{}{}", self.name(), std::env::consts::EXE_SUFFIX)
    }

    /// The program an invocation name selects: the daemon when its file name
    /// is `agentd` (`agentd.exe` on Windows, where names are compared without
    /// regard to case), the CLI for anything else, an empty name included.
    pub fn invoked_as(argv0: &OsStr) -> Role {
        match file_role(Path::new(argv0)) {
            Some(Role::Daemon) => Role::Daemon,
            _ => Role::Cli,
        }
    }

    fn other(self) -> Role {
        match self {
            Role::Cli => Role::Daemon,
            Role::Daemon => Role::Cli,
        }
    }
}

/// The program `path`'s file name names exactly, if it names one: a file
/// called `agentdocker` or `agentd` (with `.exe`, in any case, on Windows).
pub fn file_role(path: &Path) -> Option<Role> {
    let name = path.file_name()?.to_str()?;
    [Role::Cli, Role::Daemon].into_iter().find(|role| {
        if cfg!(windows) {
            let stem = name
                .len()
                .checked_sub(4)
                .filter(|&cut| name.is_char_boundary(cut))
                .filter(|&cut| name[cut..].eq_ignore_ascii_case(".exe"))
                .map_or(name, |cut| &name[..cut]);
            stem.eq_ignore_ascii_case(role.name())
        } else {
            name == role.name()
        }
    })
}

/// The program this process was invoked as, by its own `argv[0]`.
pub fn invoked() -> Role {
    static INVOKED: OnceLock<Role> = OnceLock::new();
    *INVOKED.get_or_init(|| Role::invoked_as(&std::env::args_os().next().unwrap_or_default()))
}

/// Whether `a` and `b` are one file: one inode (one file index on Windows),
/// whatever the names. False when either cannot be read.
pub fn same_file(a: &Path, b: &Path) -> bool {
    same_file::is_same_file(a, b).unwrap_or(false)
}

/// Whether `a` and `b` run the same program: one file, or two files of the
/// same bytes (a copy, as an archive that cannot hold a link ships it).
pub fn same_program(a: &Path, b: &Path) -> bool {
    if same_file(a, b) {
        return true;
    }
    let (Ok(left), Ok(right)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    if !left.is_file() || !right.is_file() || left.len() != right.len() {
        return false;
    }
    identical(a, b).unwrap_or(false)
}

fn identical(a: &Path, b: &Path) -> std::io::Result<bool> {
    use std::io::Read;
    let (mut a, mut b) = (std::fs::File::open(a)?, std::fs::File::open(b)?);
    let (mut left, mut right) = (vec![0_u8; 64 * 1024], vec![0_u8; 64 * 1024]);
    loop {
        let count = read_full(&mut a, &mut left)?;
        if count != read_full(&mut b, &mut right)? || left[..count] != right[..count] {
            return Ok(false);
        }
        if count == 0 {
            return Ok(true);
        }
    }

    fn read_full(file: &mut std::fs::File, buffer: &mut [u8]) -> std::io::Result<usize> {
        let mut filled = 0;
        while filled < buffer.len() {
            match file.read(&mut buffer[filled..])? {
                0 => break,
                count => filled += count,
            }
        }
        Ok(filled)
    }
}

/// `executable`, a path to one of the two names, spelled as `role`'s: the
/// other name beside it when that is the same file, else `executable` as it
/// is. A path to any other file is returned unchanged.
pub fn spelled_as(executable: &Path, role: Role) -> PathBuf {
    match file_role(executable) {
        Some(named) if named == role.other() => {
            let sibling = executable.with_file_name(role.file_name());
            if same_file(&sibling, executable) {
                sibling
            } else {
                executable.to_owned()
            }
        }
        _ => executable.to_owned(),
    }
}

/// How to start a program: the file to execute and the arguments that go
/// before the program's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

impl Invocation {
    /// `program` with nothing before its own arguments.
    pub fn plain(program: PathBuf) -> Self {
        Self {
            program,
            args: Vec::new(),
        }
    }

    /// The daemon from this executable under a name that is not `agentd`.
    pub fn daemon_run(program: PathBuf) -> Self {
        Self {
            program,
            args: vec!["daemon".into(), "run".into()],
        }
    }

    /// A command that starts it; the caller adds the program's arguments.
    pub fn command(&self) -> std::process::Command {
        let mut command = std::process::Command::new(&self.program);
        command.args(&self.args);
        command
    }

    /// Its argument vector, for a runner that takes one.
    pub fn argv(&self) -> Vec<String> {
        std::iter::once(self.program.as_os_str())
            .chain(self.args.iter().map(OsString::as_os_str))
            .map(|part| part.to_string_lossy().into_owned())
            .collect()
    }
}

/// How to start this executable's program, at `executable` under any name,
/// as `role`. A file named for `role` starts as it is. A file of the other
/// name starts through `role`'s name beside it when that runs the same
/// program ([`same_program`]). Otherwise the daemon is `<file> daemon run`
/// whatever the file is called, since any name but `agentd` is the CLI; and
/// the CLI is the file as it is, which every shipped layout names
/// `agentdocker`. A file that may be another program, such as an earlier
/// release's standalone `agentd` under another name, is not this function's
/// to judge: ask it (see the daemon's reload).
pub fn invocation(executable: &Path, role: Role) -> Invocation {
    let named = file_role(executable);
    if named == Some(role) {
        return Invocation::plain(executable.to_owned());
    }
    if named.is_some() {
        let sibling = executable.with_file_name(role.file_name());
        if same_program(&sibling, executable) {
            return Invocation::plain(sibling);
        }
    }
    match role {
        Role::Daemon => Invocation::daemon_run(executable.to_owned()),
        Role::Cli => Invocation::plain(executable.to_owned()),
    }
}

/// `path` resolved to the real file it names, keeping `role`'s name: a
/// symbolic `agentd` that resolves to `agentdocker` resolves to the `agentd`
/// beside it, so what is recorded still starts the daemon. Falls back to
/// `path` as given when it cannot be resolved.
pub fn resolve(path: &Path, role: Role) -> PathBuf {
    let Ok(resolved) = std::fs::canonicalize(path) else {
        return path.to_owned();
    };
    if file_role(path) != Some(role) || file_role(&resolved) == Some(role) {
        return resolved;
    }
    let respelled = spelled_as(&resolved, role);
    if file_role(&respelled) == Some(role) {
        respelled
    } else {
        // The link resolves somewhere its own name is not beside it: keep
        // the link, in its resolved directory, as the name to start.
        path.parent()
            .and_then(|parent| std::fs::canonicalize(parent).ok())
            .map_or_else(|| path.to_owned(), |parent| parent.join(role.file_name()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(role: Role) -> String {
        role.file_name()
    }

    #[test]
    fn the_invoked_name_selects_the_program() {
        for (argv0, role) in [
            ("agentd", Role::Daemon),
            ("/opt/agentdocker/bin/agentd", Role::Daemon),
            ("agentdocker", Role::Cli),
            ("/usr/local/bin/agentdocker", Role::Cli),
            ("", Role::Cli),
            ("agentd-test", Role::Cli),
            ("xagentd", Role::Cli),
            ("/agentd/agentdocker", Role::Cli),
        ] {
            assert_eq!(Role::invoked_as(OsStr::new(argv0)), role, "{argv0}");
        }
        // Windows names carry `.exe` and ignore case; elsewhere they are
        // exact.
        for argv0 in [
            r"C:\Program Files\AgentDocker\agentd.exe",
            "AGENTD.EXE",
            "Agentd",
        ] {
            assert_eq!(
                Role::invoked_as(OsStr::new(argv0)),
                if cfg!(windows) {
                    Role::Daemon
                } else {
                    Role::Cli
                },
                "{argv0}"
            );
        }
        assert_eq!(file_role(Path::new("agentdocker-ui")), None);
        assert_eq!(file_role(Path::new(&name(Role::Cli))), Some(Role::Cli));
    }

    #[test]
    fn a_hard_link_is_spelled_by_the_invoked_name_and_nothing_else_is() {
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join(name(Role::Cli));
        let daemon = dir.path().join(name(Role::Daemon));
        std::fs::write(&cli, b"one program").unwrap();
        // Before the link: nothing to respell to.
        assert_eq!(spelled_as(&cli, Role::Daemon), cli);
        std::fs::hard_link(&cli, &daemon).unwrap();
        assert_eq!(spelled_as(&cli, Role::Daemon), daemon);
        assert_eq!(spelled_as(&daemon, Role::Cli), cli);
        assert_eq!(spelled_as(&daemon, Role::Daemon), daemon);
        assert_eq!(spelled_as(&cli, Role::Cli), cli);
        // A separate copy is another file: the kernel never confuses the
        // two, so the spelling stays.
        std::fs::remove_file(&daemon).unwrap();
        std::fs::write(&daemon, b"one program").unwrap();
        assert_eq!(spelled_as(&cli, Role::Daemon), cli);
        // Any other name is left alone, even beside a link.
        let other = dir.path().join("agentdocker-ui");
        std::fs::hard_link(&cli, &other).unwrap();
        assert_eq!(spelled_as(&other, Role::Daemon), other);
    }

    #[test]
    fn the_daemon_starts_through_its_link_or_as_daemon_run() {
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join(name(Role::Cli));
        let daemon = dir.path().join(name(Role::Daemon));
        std::fs::write(&cli, b"this release").unwrap();
        let run = Invocation {
            program: cli.clone(),
            args: vec!["daemon".into(), "run".into()],
        };
        // No link: the CLI runs it.
        assert_eq!(invocation(&cli, Role::Daemon), run);
        assert_eq!(
            run.argv(),
            [cli.display().to_string(), "daemon".into(), "run".into()]
        );
        // A stale daemon from an earlier build is another program.
        std::fs::write(&daemon, b"an older release").unwrap();
        assert_eq!(invocation(&cli, Role::Daemon), run);
        // A copy of the same bytes is the same program; so is a link.
        std::fs::write(&daemon, b"this release").unwrap();
        assert_eq!(
            invocation(&cli, Role::Daemon),
            Invocation::plain(daemon.clone())
        );
        std::fs::remove_file(&daemon).unwrap();
        std::fs::hard_link(&cli, &daemon).unwrap();
        assert_eq!(
            invocation(&cli, Role::Daemon),
            Invocation::plain(daemon.clone())
        );
        assert_eq!(
            invocation(&daemon, Role::Daemon),
            Invocation::plain(daemon.clone())
        );
        assert_eq!(
            invocation(&daemon, Role::Cli),
            Invocation::plain(cli.clone())
        );
        // This executable under any other name is the CLI by that name, so
        // it runs the daemon as `daemon run`; the CLI as it is.
        let renamed = dir.path().join("candidate");
        assert_eq!(
            invocation(&renamed, Role::Daemon),
            Invocation::daemon_run(renamed.clone())
        );
        assert_eq!(
            invocation(&renamed, Role::Cli),
            Invocation::plain(renamed.clone())
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_daemon_link_resolves_to_its_own_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let real = root.join("real");
        std::fs::create_dir(&real).unwrap();
        let cli = real.join(CLI);
        std::fs::write(&cli, b"one program").unwrap();
        // `agentd -> agentdocker` beside it, as a hand-made link.
        let beside = real.join(DAEMON);
        std::os::unix::fs::symlink(CLI, &beside).unwrap();
        assert_eq!(resolve(&beside, Role::Daemon), beside);
        // A link elsewhere that names the daemon resolves to the one beside
        // the file, which is the same program.
        let bin = root.join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::os::unix::fs::symlink(&beside, bin.join(DAEMON)).unwrap();
        assert_eq!(resolve(&bin.join(DAEMON), Role::Daemon), beside);
        // A hard link resolves to itself; so does the CLI.
        std::fs::remove_file(&beside).unwrap();
        std::fs::hard_link(&cli, &beside).unwrap();
        assert_eq!(resolve(&bin.join(DAEMON), Role::Daemon), beside);
        assert_eq!(resolve(&cli, Role::Cli), cli);
        // A daemon link straight to a CLI with no daemon name beside it
        // keeps its own name, in its resolved directory.
        let lone = root.join("lone");
        std::fs::create_dir(&lone).unwrap();
        std::fs::copy(&cli, lone.join(CLI)).unwrap();
        std::os::unix::fs::symlink(lone.join(CLI), bin.join("other")).unwrap();
        std::fs::remove_file(bin.join(DAEMON)).unwrap();
        std::os::unix::fs::symlink(lone.join(CLI), bin.join(DAEMON)).unwrap();
        assert_eq!(resolve(&bin.join(DAEMON), Role::Daemon), bin.join(DAEMON));
    }
}
