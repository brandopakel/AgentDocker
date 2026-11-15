//! Explicit worktree creation and verified, uncommitted integration.
use super::*;
use agentdocker_core::Validation;
use agentdocker_host::{command, content};

pub(super) async fn git(root: PathBuf, args: Vec<String>) -> anyhow::Result<command::Output> {
    tokio::task::spawn_blocking(move || {
        let argv = std::iter::once("git".to_owned())
            .chain(args)
            .collect::<Vec<_>>();
        command::run(&root, &argv, std::time::Duration::from_secs(30))
    })
    .await?
    .map_err(Into::into)
}

/// Holds a checkout marked as "the daemon is committing here" for as
/// long as it lives, and lets go however the commit ends.
///
/// Dropping it takes the state lock, so it must never be dropped by a
/// holder of a state guard: every hold taken while one lives is a block
/// or a statement of its own that ends before any return.
pub(super) struct Committing<'a> {
    daemon: &'a Daemon,
    root: PathBuf,
}

impl<'a> Committing<'a> {
    pub(super) fn mark(daemon: &'a Daemon, root: PathBuf) -> Self {
        lock(&daemon.state).committing.insert(root.clone());
        Self { daemon, root }
    }
}

impl Drop for Committing<'_> {
    fn drop(&mut self) {
        lock(&self.daemon.state).committing.remove(&self.root);
    }
}

/// Holds a linked worktree marked as being removed for as long as it
/// lives, and lets go however the removal ends. The mark itself is made
/// by the step that found the worktree idle, under the state lock it
/// looked under, so that being idle and being reserved are one step.
///
/// Dropping it takes the state lock, so it must never be dropped by a
/// holder of a state guard.
struct Removing<'a> {
    daemon: &'a Daemon,
    path: PathBuf,
}

impl Drop for Removing<'_> {
    fn drop(&mut self) {
        lock(&self.daemon.state).removing.remove(&self.path);
    }
}

/// The subject of a commit message: what a journal line can carry.
fn first_line(message: &str) -> &str {
    message.lines().next().unwrap_or("").trim()
}

/// A project's worktrees folder: the one place inside the project a
/// linked checkout may sit.
pub(super) struct Home {
    /// The project's main root.
    pub project_root: PathBuf,
    /// The folder, with every link on its way resolved: inside the
    /// project it has none ([`through_a_link`]).
    pub dir: PathBuf,
}

impl Home {
    /// Whether `path` is a place in the folder, and not the folder.
    fn holds(&self, path: &Path) -> bool {
        path != self.dir && path.starts_with(&self.dir)
    }
}

/// `git worktree add -b <branch> <path> HEAD` in `root`, for a new path
/// and a branch name git accepts. The path is outside the checkout and
/// outside the project, or inside the project's own worktrees folder
/// (`home`), in a folder of its own there: nowhere else in a checkout,
/// where its files would be the checkout's own untracked files. Shared by
/// `worktree-create` and `run --isolate`.
pub(super) async fn add_worktree(
    root: PathBuf,
    path: &Path,
    branch: &str,
    from: Option<&str>,
    home: Option<&Home>,
) -> Result<(), Box<Response>> {
    let nested =
        path.starts_with(&root) || home.is_some_and(|home| path.starts_with(&home.project_root));
    let in_home = home.is_some_and(|home| home.holds(path));
    // A checkout in the worktrees folder holds no other: the new one's
    // files would be its untracked files.
    let in_another = home.filter(|_| in_home).is_some_and(|home| {
        path.ancestors()
            .skip(1)
            .take_while(|folder| *folder != home.dir)
            .any(|folder| folder.join(".git").exists())
    });
    if in_another {
        return Err(Box::new(Response::invalid(
            "worktree path is inside another checkout of the worktrees folder: give it a folder of its own",
        )));
    }
    if path.exists() || (nested && !in_home) {
        return Err(Box::new(Response::invalid(match home {
            Some(home) if home.dir.starts_with(&home.project_root) => format!(
                "worktree path must be new, and outside the checkout or inside the project's worktrees folder ({})",
                home.dir.display()
            ),
            _ => "worktree path must be new and outside the current checkout".to_owned(),
        })));
    }
    match git(
        root.clone(),
        vec![
            "check-ref-format".into(),
            "--branch".into(),
            branch.to_owned(),
        ],
    )
    .await
    {
        Ok(output) if output.success && !branch.starts_with('-') => {}
        _ => return Err(Box::new(Response::invalid("invalid branch name"))),
    }
    // The start point is a ref, never an option: `--` ends the flags.
    let start = match from {
        Some(from) if from.starts_with('-') || from.trim().is_empty() => {
            return Err(Box::new(Response::invalid("invalid start point")));
        }
        Some(from) => from.to_owned(),
        None => "HEAD".to_owned(),
    };
    match git(
        root,
        vec![
            "worktree".into(),
            "add".into(),
            "-b".into(),
            branch.to_owned(),
            "--".into(),
            path.to_string_lossy().into_owned(),
            start,
        ],
    )
    .await
    {
        Ok(output) if output.success => Ok(()),
        Ok(output) => Err(Box::new(Response::invalid(output.text))),
        Err(e) => Err(Box::new(Response::invalid(e))),
    }
}

/// The file a project keeps its worktree includes in: one git pathspec per
/// line of files git ignores that a worktree still needs (`.env`, local
/// settings). The name is the one other session managers read.
pub const INCLUDE_FILE: &str = ".worktreeinclude";
/// At most this much is copied into one worktree.
const INCLUDE_BYTES: u64 = 256 * 1024 * 1024;

/// Copy the files `.worktreeinclude` names from `root` into `path`: only
/// files git ignores or does not track there (tracked files are in the
/// worktree already), each under the same relative path. Says how many
/// were copied and the first failure, and never fails the worktree: a
/// missing `.env` is the agent's to notice, a half-made worktree is not.
pub(super) async fn copy_includes(root: PathBuf, path: PathBuf) -> (usize, Option<String>) {
    let Ok(text) = std::fs::read_to_string(root.join(INCLUDE_FILE)) else {
        return (0, None);
    };
    // Only a copy that opens each folder without following a symlink is
    // safe in a checkout someone else's commit made; there is none here
    // yet on Windows, so nothing is copied there rather than copied unsafely.
    if cfg!(not(unix)) {
        return (
            0,
            Some(format!(
                "{INCLUDE_FILE} is not copied on this platform yet; copy what it names by hand"
            )),
        );
    }
    let specs: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('-'))
        .map(str::to_owned)
        .collect();
    if specs.is_empty() {
        return (0, None);
    }
    let mut args = vec![
        "ls-files".to_owned(),
        "-z".to_owned(),
        "--others".to_owned(),
        "--ignored".to_owned(),
        "--exclude-standard".to_owned(),
        "--".to_owned(),
    ];
    args.extend(specs);
    let listed = match git(root.clone(), args).await {
        Ok(output) if output.success => output.stdout,
        Ok(output) => return (0, Some(output.text)),
        Err(e) => return (0, Some(e.to_string())),
    };
    let (mut copied, mut bytes, mut trouble) = (0usize, 0u64, None);
    for relative in listed.split('\0').filter(|r| !r.is_empty()) {
        let from = root.join(relative);
        let to = path.join(relative);
        // A symlink is left out: it could point anywhere.
        let Ok(meta) = std::fs::symlink_metadata(&from) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        bytes += meta.len();
        if bytes > INCLUDE_BYTES {
            trouble.get_or_insert_with(|| {
                format!("{INCLUDE_FILE} names more than 256 MiB; the rest was not copied")
            });
            break;
        }
        // The worktree is a checkout, and a checkout can carry a symlink
        // where this file or a folder on its way goes: written through, the
        // copy would land wherever it points. Nothing is written through
        // one, and nothing already there is replaced.
        if !stays_inside(&path, relative) {
            trouble.get_or_insert_with(|| {
                format!("{relative}: the worktree has a symlink on its way; not copied")
            });
            continue;
        }
        match copy_new(&from, &path, relative, &to, &meta) {
            Ok(()) => copied += 1,
            Err(e) => {
                trouble.get_or_insert_with(|| format!("{relative}: {e}"));
            }
        }
    }
    (copied, trouble)
}

/// Whether writing `relative` under `base` stays under it: an ordinary
/// path, and no part of the way there, the file itself included, a
/// symlink.
fn stays_inside(base: &Path, relative: &str) -> bool {
    let mut at = base.to_path_buf();
    for part in Path::new(relative).components() {
        let std::path::Component::Normal(name) = part else {
            return false;
        };
        at.push(name);
        if std::fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink()) {
            return false;
        }
    }
    true
}

/// Copy `from` to a file made new at `relative` beneath `base`. On Unix
/// every folder on the way is opened without following a symlink, so one
/// swapped in after `stays_inside` looked is refused, and the file is
/// owner-only until its bytes are in. Elsewhere the file is still made new,
/// never over anything there.
#[cfg(unix)]
fn copy_new(
    from: &Path,
    base: &Path,
    relative: &str,
    _to: &Path,
    meta: &std::fs::Metadata,
) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut source = std::fs::File::open(from)?;
    agentdocker_host::files::copy_new_beneath(
        base,
        Path::new(relative),
        &mut source,
        meta.permissions().mode(),
    )
    .map(|_| ())
}

#[cfg(not(unix))]
fn copy_new(
    _from: &Path,
    _base: &Path,
    _relative: &str,
    _to: &Path,
    _meta: &std::fs::Metadata,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "includes are not copied on this platform",
    ))
}

/// How long seeding one worktree's build directories may take, all of
/// them together. A clone takes well under a second; this bounds copies.
const SEED_LIMIT: std::time::Duration = std::time::Duration::from_secs(120);

/// Seed the new worktree `path` with `names` from the checkout `root`, on a
/// blocking thread: see [`agentdocker_host::seed`].
async fn seed_build_dirs(
    root: PathBuf,
    path: PathBuf,
    names: Vec<String>,
) -> Vec<agentdocker_core::seed::Seeded> {
    if names.is_empty() {
        return Vec::new();
    }
    let asked = names.clone();
    match tokio::task::spawn_blocking(move || {
        agentdocker_host::seed::seed(&root, &path, &names, SEED_LIMIT)
    })
    .await
    {
        Ok(seeded) => seeded,
        Err(e) => asked
            .into_iter()
            .map(|dir| agentdocker_core::seed::Seeded {
                dir,
                outcome: agentdocker_core::seed::Outcome::Failed,
                note: Some(format!("seeding stopped: {e}")),
            })
            .collect(),
    }
}

/// Remove a session's linked worktree once it is done with it: the session
/// has ended, the worktree is clean (untracked files included), and git
/// agrees to a non-force removal. The branch is deleted only when git
/// says it is merged (`branch -d`); otherwise it stays, and the reply
/// says so.
pub(super) async fn remove_finished(
    root: PathBuf,
    path: PathBuf,
) -> Result<(bool, Option<String>), String> {
    match git(
        path.clone(),
        vec![
            "status".into(),
            "--porcelain".into(),
            "--untracked-files=all".into(),
        ],
    )
    .await
    {
        Ok(output) if output.success && output.stdout.trim().is_empty() => {}
        Ok(output) if output.success => {
            return Err(
                "the worktree has uncommitted or untracked files; commit or discard them first"
                    .into(),
            );
        }
        Ok(output) => return Err(output.text),
        Err(e) => return Err(e.to_string()),
    }
    let branch = match git(
        path.clone(),
        vec![
            "symbolic-ref".into(),
            "--short".into(),
            "-q".into(),
            "HEAD".into(),
        ],
    )
    .await
    {
        Ok(output) if output.success => {
            Some(output.stdout.trim().to_owned()).filter(|b| !b.is_empty())
        }
        _ => None,
    };
    match git(
        root.clone(),
        vec![
            "worktree".into(),
            "remove".into(),
            "--".into(),
            path.to_string_lossy().into_owned(),
        ],
    )
    .await
    {
        Ok(output) if output.success => {}
        Ok(output) => return Err(output.text),
        Err(e) => return Err(e.to_string()),
    }
    let Some(branch) = branch else {
        return Ok((false, Some("the worktree had no branch of its own".into())));
    };
    match git(
        root,
        // Preserve Git's own deletion report and full object ID. A prior
        // branch review can name a different branch/tip by the time this runs.
        vec![
            "-c".into(),
            "core.abbrev=no".into(),
            "branch".into(),
            "-d".into(),
            "--".into(),
            branch.clone(),
        ],
    )
    .await
    {
        Ok(output) if output.success => Ok((
            true,
            Some(output.stdout.trim().to_owned()).filter(|text| !text.is_empty()),
        )),
        Ok(output) => Ok((
            false,
            Some(format!("branch {branch} is kept: {}", output.text.trim())),
        )),
        Err(e) => Ok((false, Some(e.to_string()))),
    }
}

/// One checkout as `git worktree list --porcelain -z` names it: where it
/// is, and the branch checked out there when it has one.
#[derive(Debug, PartialEq, Eq)]
struct Listed {
    path: PathBuf,
    branch: Option<String>,
}

/// The checkouts of a repository from `git worktree list --porcelain -z`,
/// the main one first, as git lists them.
fn listed(porcelain: &str) -> Vec<Listed> {
    let mut all: Vec<Listed> = Vec::new();
    for line in porcelain.split('\0') {
        if let Some(path) = line.strip_prefix("worktree ") {
            all.push(Listed {
                path: project::canonical(Path::new(path)),
                branch: None,
            });
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/")
            && let Some(last) = all.last_mut()
        {
            last.branch = Some(branch.to_owned());
        }
    }
    all
}

/// Remove only a clean, unstarted isolated checkout whose branch did not advance.
/// Git's normal (non-force) removal/deletion checks remain authoritative.
pub(super) async fn cleanup_unstarted(record: &AgentRecord) -> (bool, bool, Option<String>) {
    let Some(path) = record.spec.workdir.clone() else {
        return (false, false, Some("missing checkout path".into()));
    };
    let Some(project) = &record.project else {
        return (false, false, Some("missing repository identity".into()));
    };
    let Some(vcs) = &record.vcs else {
        return (
            false,
            false,
            Some("missing original branch identity".into()),
        );
    };
    let (Some(head), Some(branch)) = (&vcs.head, &vcs.branch) else {
        return (
            false,
            false,
            Some("missing original branch identity".into()),
        );
    };
    let checks = [
        (
            path.clone(),
            vec![
                "status".into(),
                "--porcelain".into(),
                "--untracked-files=all".into(),
                "--ignored".into(),
            ],
            String::new(),
        ),
        (
            path.clone(),
            vec!["rev-parse".into(), "HEAD".into()],
            head.clone(),
        ),
        (
            path.clone(),
            vec!["symbolic-ref".into(), "--short".into(), "HEAD".into()],
            branch.clone(),
        ),
        (
            project.root.clone(),
            vec!["rev-parse".into(), "HEAD".into()],
            head.clone(),
        ),
    ];
    for (root, args, expected) in checks {
        match git(root, args).await {
            Ok(output) if output.success && output.stdout.trim() == expected => {}
            _ => {
                return (
                    false,
                    false,
                    Some("checkout or branch changed; retained for inspection".into()),
                );
            }
        }
    }
    match git(
        project.root.clone(),
        vec![
            "worktree".into(),
            "remove".into(),
            "--".into(),
            path.to_string_lossy().into_owned(),
        ],
    )
    .await
    {
        Ok(output) if output.success => {}
        _ => {
            return (
                false,
                false,
                Some("Git refused non-force worktree removal".into()),
            );
        }
    }
    match git(
        project.root.clone(),
        vec!["branch".into(), "-d".into(), "--".into(), branch.clone()],
    )
    .await
    {
        Ok(output) if output.success => (true, true, None),
        _ => (
            true,
            false,
            Some("worktree removed; Git retained the branch".into()),
        ),
    }
}

/// The line that keeps a project's worktrees folder out of git, when the
/// folder is inside the project: rooted at the repository's top, as
/// `info/exclude` reads it, and naming that folder and no other. A folder
/// name is taken to the letter, so what git would read as a pattern
/// (`*`, `?`, `[`, `]`, `\`) is escaped; a name no line can hold (a
/// control character, or one that is not text) is said.
fn exclude_line(project_root: &Path, home: &Path) -> Result<Option<String>, String> {
    let Ok(relative) = home.strip_prefix(project_root) else {
        return Ok(None);
    };
    let mut line = String::new();
    for part in relative.components() {
        let name = part
            .as_os_str()
            .to_str()
            .filter(|name| !name.chars().any(char::is_control))
            .ok_or_else(|| {
                format!(
                    "{} has a name no git ignore rule can hold; name another folder with `[worktrees] dir`",
                    home.display()
                )
            })?;
        line.push('/');
        for letter in name.chars() {
            if matches!(letter, '\\' | '*' | '?' | '[' | ']') {
                line.push('\\');
            }
            line.push(letter);
        }
    }
    Ok((!line.is_empty()).then(|| line + "/"))
}

/// Whether git leaves the worktrees folder alone, in git's own word.
/// The line in `info/exclude` is the least of git's ignore rules: one in
/// a `.gitignore` the project tracks (`!*/`) wins over it, and the
/// checkouts in the folder would then read as the project's untracked
/// files, to be staged by the next `git add -A`. So the folder is asked
/// about once it exists and the line is there.
///
/// The answer is git's exit status and nothing read out of its text:
/// `check-ignore -q` succeeds for an ignored path and fails, silently,
/// for one that is not. The rule that keeps the folder in view is then
/// shown as git prints it (`check-ignore -v`), for the person to read:
/// its file may hold a colon or a number, so it is never taken apart.
async fn kept_out_of_git(project_root: &Path, home: &Path) -> Result<(), String> {
    let ask = |how: &str| {
        git(
            project_root.to_path_buf(),
            vec![
                "check-ignore".into(),
                how.to_owned(),
                "--".into(),
                home.to_string_lossy().into_owned(),
            ],
        )
    };
    let around = "name another folder with `[worktrees] dir` in agentd.toml (an absolute one is outside every project), or give the worktree a path of its own";
    let answer = ask("-q").await.map_err(|e| {
        format!(
            "git could not say whether {} is ignored: {e}",
            home.display()
        )
    })?;
    if answer.success {
        return Ok(());
    }
    // Not ignored is said with no word; anything said is why git could
    // not answer.
    let said = answer.text.trim();
    if !said.is_empty() {
        return Err(format!(
            "git could not say whether {} is ignored ({said}): {around}",
            home.display()
        ));
    }
    let rule = ask("-v").await.ok().and_then(|shown| {
        let line = shown.stdout.lines().next()?;
        let rule = line.split('\t').next()?.trim();
        (!rule.is_empty()).then(|| rule.to_owned())
    });
    Err(match rule {
        Some(rule) => format!(
            "git does not ignore {}, so a checkout there would read as the project's own untracked files. The rule that keeps it in view, as git names it (file, line, rule): `{rule}`. Change that rule, or {around}",
            home.display()
        ),
        None => format!(
            "git does not ignore {}, so a checkout there would read as the project's own untracked files: {around}",
            home.display()
        ),
    })
}

/// The folder a project's worktrees go in is reached through real
/// folders only when it is inside the project: a checkout someone else's
/// commit made can hold a link where the folder goes, and the checkouts
/// would land wherever it points.
fn through_a_link(project_root: &Path, home: &Path) -> Option<String> {
    let real = project::canonical(home);
    (home.starts_with(project_root) && real != home).then(|| {
        format!(
            "{} is reached through a symbolic link (to {}), and worktrees are not made through one: remove the link, or name another folder with `[worktrees] dir`",
            home.display(),
            real.display()
        )
    })
}

/// Make the worktrees folder and, when it is inside the project, keep it
/// out of git through the repository's own `info/exclude`: local to this
/// clone, shared by its worktrees, and never the project's `.gitignore`,
/// which is the project's to write. Without the line every checkout in
/// the folder would read as the main checkout's untracked files. The
/// line is added to the end of the file, whose other lines are left as
/// they are. Answers the folders it made, the deepest first, for a
/// caller that finds the place unusable after all.
fn prepare_home(project_root: &Path, home: &Path) -> Result<Vec<PathBuf>, String> {
    // One at a time: two checkouts asked for at once would both find the
    // line missing and write it twice.
    static ONE: Mutex<()> = Mutex::new(());
    let _one = lock(&ONE);
    if let Some(why) = through_a_link(project_root, home) {
        return Err(why);
    }
    let line = exclude_line(project_root, home)?;
    let made: Vec<PathBuf> = home
        .ancestors()
        .take_while(|folder| *folder != project_root)
        .filter(|folder| !folder.exists())
        .map(Path::to_path_buf)
        .collect();
    std::fs::create_dir_all(home).map_err(|e| format!("cannot create {}: {e}", home.display()))?;
    let Some(line) = line else {
        return Ok(Vec::new());
    };
    // Looked at again now that it exists: what was missing a moment ago
    // may have been made a link meanwhile.
    if let Some(why) = through_a_link(project_root, home) {
        return Err(why);
    }
    let Some((_, common)) = vcs::git_dirs(project_root) else {
        return Err(format!("{} is not a git checkout", project_root.display()));
    };
    let file = common.join("info").join("exclude");
    let known = std::fs::read_to_string(&file).unwrap_or_default();
    if known.lines().any(|known| known.trim() == line) {
        return Ok(made);
    }
    let mut text = String::new();
    if !known.is_empty() && !known.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("# Checkouts AgentDocker makes for this project's agents.\n");
    text.push_str(&line);
    text.push('\n');
    if let Some(folder) = file.parent() {
        std::fs::create_dir_all(folder)
            .map_err(|e| format!("cannot create {}: {e}", folder.display()))?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .and_then(|mut file| std::io::Write::write_all(&mut file, text.as_bytes()))
        .map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    Ok(made)
}

impl Daemon {
    /// Where the checkouts made for a project's agents go: inside the
    /// project unless the daemon's settings name another place
    /// (`[worktrees] dir`). Nothing is made here. A settings file that
    /// cannot be read is said and passed over, as elsewhere; a setting
    /// that names no usable place is refused with its reason.
    pub(super) fn worktree_place(&self, project: &ProjectRef) -> Result<Home, Box<Response>> {
        let settings = match crate::config::read(&self.home) {
            Ok(config) => config.worktrees,
            Err(error) => {
                warn!(%error, "agentd.toml could not be read; worktrees go inside each project");
                Default::default()
            }
        };
        let project_root = project::canonical(&project.root);
        let named = settings
            .home(&project_root, project.id().short())
            .map_err(|e| Box::new(Response::invalid(e)))?;
        if let Some(why) = through_a_link(&project_root, &named) {
            return Err(Box::new(Response::invalid(why)));
        }
        Ok(Home {
            dir: project::canonical(&named),
            project_root,
        })
    }

    /// Make a project's worktrees folder and keep it out of git: asked
    /// only for a checkout that goes in it, so a project whose worktrees
    /// all have a path of their own never gets the folder. A folder git
    /// would not leave alone is refused, with the rule that says so, and
    /// what was made for it is taken away again.
    pub(super) async fn worktree_make(&self, home: &Home) -> Result<(), Box<Response>> {
        let (root, dir) = (home.project_root.clone(), home.dir.clone());
        let made = match tokio::task::spawn_blocking(move || prepare_home(&root, &dir)).await {
            Ok(Ok(made)) => made,
            Ok(Err(why)) => {
                return Err(Box::new(Response::error(
                    ErrorCode::StorageUnavailable,
                    why,
                )));
            }
            Err(e) => return Err(Box::new(Response::internal(e))),
        };
        if !home.dir.starts_with(&home.project_root) {
            return Ok(());
        }
        if let Err(why) = kept_out_of_git(&home.project_root, &home.dir).await {
            // Only what this call made, and only while it is empty.
            for folder in made {
                let _ = std::fs::remove_dir(folder);
            }
            return Err(Box::new(Response::invalid(why)));
        }
        Ok(())
    }

    /// A linked checkout on a new branch, from this session's HEAD or
    /// from `from` (a branch, tag or commit of the same repository). The
    /// daemon remembers who asked, so commits made there with git alone
    /// are still that agent's in the journal, and gives it a post-commit
    /// hook that reports each of them, so they are kept as authorship
    /// (`authorship.rs`).
    ///
    /// `seed_build` names directories of the agent's checkout (build
    /// output such as `target` or `node_modules`) to clone or copy into
    /// the new one, once it exists and off the state lock; what became of
    /// each is in the answer, and none of it fails the worktree.
    pub(super) async fn worktree_create(
        &self,
        reference: &str,
        path: Option<String>,
        branch: String,
        from: Option<String>,
        seed_build: Vec<String>,
    ) -> Response {
        // A bad name is the caller's to fix before anything is made.
        if let Err(e) = agentdocker_core::seed::check(&seed_build) {
            return Response::invalid(e);
        }
        let (agent, root, _) = match self.reader_checkout(reference) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        // The project's worktrees folder: where a checkout goes when no
        // path is given, and the one place inside the project a path may
        // name.
        let project = lock(&self.state)
            .registry
            .get(&agent)
            .and_then(|record| record.project.clone())
            .filter(|project| project.source == ProjectSource::Git);
        let home = match project.as_ref().map(|project| self.worktree_place(project)) {
            Some(Ok(home)) => Some(home),
            // A named path does not need the folder; no path does.
            Some(Err(response)) if path.is_none() => return *response,
            Some(Err(_)) | None => None,
        };
        let named = path.is_some();
        let path = match (path, &home) {
            (Some(path), _) => PathBuf::from(path),
            (None, Some(home)) => home.dir.join(paths::worktree_folder(&branch)),
            (None, None) => {
                return Response::invalid(
                    "name a path: this agent's checkout is not a git project with a worktrees folder of its own",
                );
            }
        };
        let path = match physical(path).await {
            Ok(p) => p,
            Err(e) => return Response::invalid(e),
        };
        if !named && path.exists() {
            return Response::invalid(format!(
                "{} is there already: name another branch, or a path",
                path.display()
            ));
        }
        if let Some(home) = home.as_ref().filter(|home| home.holds(&path))
            && let Err(response) = self.worktree_make(home).await
        {
            return *response;
        }
        if let Err(response) =
            add_worktree(root.clone(), &path, &branch, from.as_deref(), home.as_ref()).await
        {
            return *response;
        }
        {
            let mut state = lock(&self.state);
            state.worktree_creators.insert(path.clone(), agent.clone());
            state.emit(EventKind::WorktreeCreated {
                agent: agent.clone(),
                path: path.clone(),
            });
        }
        let unwitnessed = self.witness_worktree(&path, &agent).await;
        let seeded = seed_build_dirs(root, path.clone(), seed_build).await;
        Response::Worktree {
            path,
            branch,
            seeded,
            unwitnessed,
        }
    }

    /// Remove an ended session's linked worktree once its branch is merged
    /// or no longer wanted; the worktree must be clean. Emits
    /// `worktree_cleanup` as the unstarted cleanup does.
    pub(super) async fn worktree_remove(&self, reference: &str) -> Response {
        let (id, path, root) = {
            let mut state = lock(&self.state);
            let id = match state.resolve(reference) {
                Ok(id) => id,
                Err(e) => return *e,
            };
            let record = state.registry.get(&id).expect("resolved").clone();
            if record.status.is_live() {
                return Response::error(
                    ErrorCode::Conflict,
                    "the session is still running; stop it before removing its worktree",
                );
            }
            let Some(project) = &record.project else {
                return Response::error(ErrorCode::Invalid, "this session works in no project");
            };
            let Some(worktree) = &project.worktree else {
                return Response::error(
                    ErrorCode::Invalid,
                    "this session works in the project's main folder, which is not removed",
                );
            };
            (
                id,
                project::canonical(worktree),
                project::canonical(&project.root),
            )
        };
        if !path.exists() {
            return Response::error(
                ErrorCode::NotFound,
                format!("{} is already gone", path.display()),
            );
        }
        match remove_finished(root, path.clone()).await {
            Ok((branch_removed, reason)) => {
                lock(&self.state).emit(EventKind::WorktreeCleanup {
                    agent: id,
                    path: path.clone(),
                    worktree_removed: true,
                    branch_removed,
                    reason: reason.clone(),
                });
                Response::WorktreeRemoved {
                    path,
                    branch_removed,
                    note: reason,
                }
            }
            Err(reason) => Response::error(ErrorCode::Conflict, reason),
        }
    }

    /// Remove a linked worktree of the asking agent's project that nobody
    /// works in, named by its path or by the branch checked out in it.
    ///
    /// Any running agent of the project may ask, not only the one that
    /// made the worktree: sessions end, and a rule naming the maker would
    /// leave a checkout nobody is allowed to remove. What protects the
    /// work is the same as for an ended session's worktree: it must be
    /// clean, git removes it without force, and its branch goes only when
    /// git considers it merged. What protects the others is said first:
    /// the main folder is never removed, nor the checkout the asker
    /// stands in, nor one a live agent works in, nor one in which
    /// anybody holds a path or whose branch anybody holds.
    ///
    /// Git takes several commands to remove a worktree, and the state
    /// lock is not held across them. So the look that finds the worktree
    /// idle also marks it as being removed ([`Removing`]), and until the
    /// removal has ended a record that would come to work there, by any
    /// of the ways one comes to life ([`State::removal_refuses_record`]),
    /// and a claim in it, over it or on its branch are refused: nobody is
    /// told they have a checkout that is about to go.
    pub(super) async fn worktree_remove_named(
        &self,
        reference: &str,
        path: Option<String>,
        branch: Option<String>,
    ) -> Response {
        let (agent, checkout, _) = match self.reader_checkout(reference) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        let path = match (path, &branch) {
            (Some(path), None) => match physical(PathBuf::from(path)).await {
                Ok(path) => Some(path),
                Err(e) => return Response::invalid(e),
            },
            (None, Some(_)) => None,
            _ => {
                return Response::invalid(
                    "name the worktree by its path or by its branch, one of the two",
                );
            }
        };
        let listing = match git(
            checkout.clone(),
            vec![
                "worktree".into(),
                "list".into(),
                "--porcelain".into(),
                "-z".into(),
            ],
        )
        .await
        {
            Ok(output) if output.success => listed(&output.stdout),
            Ok(output) => return Response::invalid(output.text),
            Err(e) => return Response::invalid(e),
        };
        let Some(main) = listing.first().map(|main| main.path.clone()) else {
            return Response::invalid("this agent's checkout is not a git repository's");
        };
        let found = listing.iter().find(|listed| match (&path, &branch) {
            (Some(path), _) => listed.path == *path,
            (None, branch) => listed.branch.is_some() && listed.branch == *branch,
        });
        let Some(target) = found else {
            return Response::error(
                ErrorCode::NotFound,
                match (&path, &branch) {
                    (Some(path), _) => {
                        format!("no worktree of this project is at {}", path.display())
                    }
                    (None, branch) => format!(
                        "no worktree of this project has {} checked out",
                        branch.as_deref().unwrap_or_default()
                    ),
                },
            );
        };
        let target_branch = target.branch.clone();
        let target = target.path.clone();
        if target == main {
            return Response::invalid("that is the project's main folder, which is not removed");
        }
        if checkout.starts_with(&target) {
            return Response::error(
                ErrorCode::Conflict,
                "you are working in that worktree: ask from another checkout once you have left it",
            );
        }
        // Found idle and reserved in one step: from here until the
        // removal has ended, whichever way, nobody new comes to work
        // there and nothing there, nor its branch, is claimed.
        let _removing = {
            let mut state = lock(&self.state);
            state.expire_leases();
            if state.removing.contains_key(&target) {
                return Response::error(
                    ErrorCode::Conflict,
                    "that worktree is being removed already",
                );
            }
            // Working there, or about to be again: a record waiting for
            // its restart is not live yet, and its process is spawned
            // before it is, so it is counted here rather than met later.
            let working = state.registry.all().find(|record| {
                record.id != agent
                    && (record.status.is_live() || state.pending_restarts.contains(&record.id))
                    && record.project.as_ref().is_some_and(|project| {
                        project::canonical(project.dir()).starts_with(&target)
                    })
            });
            if let Some(working) = working {
                return Response::error(
                    ErrorCode::Conflict,
                    format!(
                        "{} ({}) is {} in that worktree",
                        working.spec.name,
                        working.id.short(),
                        if working.status.is_live() {
                            "working"
                        } else {
                            "about to be restarted"
                        }
                    ),
                );
            }
            // A lease on a path there, or on its branch, is somebody's
            // word that it is in use, the asker's own included: it is
            // theirs to release first, rather than left behind on a path
            // or a branch that is gone.
            let key = ResourceKey::new(format!("path:{}", target.display()));
            let on_branch = target_branch
                .as_ref()
                .map(|branch| ResourceKey::new(format!("branch:{branch}")));
            let held = state
                .leases
                .holders_of(&key)
                .into_iter()
                .next()
                .or_else(|| {
                    on_branch
                        .as_ref()
                        .and_then(|key| state.leases.holders_of(key).into_iter().next())
                });
            if let Some(lease) = held {
                let who = if lease.holder == agent {
                    "you hold".to_owned()
                } else {
                    let name = state
                        .registry
                        .get(&lease.holder)
                        .map(|record| record.spec.name.clone())
                        .unwrap_or_else(|| lease.holder.short().to_owned());
                    format!("{name} holds")
                };
                return Response::error(
                    ErrorCode::Conflict,
                    format!(
                        "{who} {}: it is released before the worktree goes",
                        lease.resource
                    ),
                );
            }
            state.removing.insert(target.clone(), target_branch.clone());
            Removing {
                daemon: self,
                path: target.clone(),
            }
        };
        #[cfg(test)]
        {
            let hold = lock(&self.removal_hold).take();
            if let Some(hold) = hold {
                hold.wait().await;
                hold.wait().await;
            }
        }
        match remove_finished(main, target.clone()).await {
            Ok((branch_removed, reason)) => {
                let mut state = lock(&self.state);
                state.worktree_creators.remove(&target);
                state.emit(EventKind::WorktreeRemoved {
                    by: agent,
                    path: target.clone(),
                    branch: target_branch,
                    branch_removed,
                    reason: reason.clone(),
                });
                Response::WorktreeRemoved {
                    path: target,
                    branch_removed,
                    note: reason,
                }
            }
            Err(reason) => Response::error(ErrorCode::Conflict, reason),
        }
    }

    pub(super) async fn worktree_diff(&self, reference: &str) -> Response {
        let (_, root, _) = match self.reader_checkout(reference) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        match git(
            root,
            vec![
                "diff".into(),
                "--no-ext-diff".into(),
                "--stat".into(),
                "--patch".into(),
                "HEAD".into(),
                "--".into(),
            ],
        )
        .await
        {
            Ok(output) if output.success => Response::Diff { text: output.text },
            Ok(output) => Response::invalid(output.text),
            Err(e) => Response::invalid(e),
        }
    }

    /// Commit an agent's checkout, and say in the journal that this
    /// agent did it.
    ///
    /// The watcher already notices a HEAD that moved and writes a
    /// `commit` entry for it, but it has to *guess* whose it was — the
    /// only agent in the checkout, else whoever holds the `branch:`
    /// lease, else nobody. Going through the daemon removes the guess:
    /// the agent asked, so the agent is who it is attributed to, and
    /// the message is the one it wrote rather than a summary of a sha.
    ///
    /// Nothing is written into the commit itself. The author stays
    /// whoever git is configured as, and no trailer is added: this is
    /// somebody's repository, and which agent typed it is our record to
    /// keep, not a change to their history.
    pub(super) async fn commit(
        &self,
        reference: &str,
        message: String,
        all: bool,
        push: bool,
    ) -> Response {
        let (agent, root, _) = match self.writer_checkout(reference) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        if message.trim().is_empty() {
            return Response::invalid("a commit needs a message");
        }
        {
            let mut state = lock(&self.state);
            let Some(record) = state.registry.get(&agent) else {
                return Response::error(ErrorCode::NotFound, "agent was removed before commit");
            };
            let policy_root = record
                .project
                .as_ref()
                .map(|p| p.dir())
                .unwrap_or(&root)
                .to_path_buf();
            let action = format!("commit:{}", policy_root.display());
            let ruling = state.permits(&agent, &action);
            if !ruling.is_allowed() {
                return state.refuse(&agent, &action, ruling);
            }
            if push {
                let action = format!("push:{}", policy_root.display());
                let ruling = state.permits(&agent, &action);
                if !ruling.is_allowed() {
                    return state.refuse(&agent, &action, ruling);
                }
            }
        }

        // What would go in, before it goes in: `--porcelain` after the
        // commit says nothing, and the count is what the journal reports.
        let staged = match git(root.clone(), {
            let mut args = vec!["diff".into(), "--name-only".into(), "--cached".into()];
            if all {
                // With `-a` the commit will also take tracked files
                // that were only modified in the worktree.
                args = vec!["diff".into(), "--name-only".into(), "HEAD".into()];
            }
            args
        })
        .await
        {
            Ok(output) if output.success => output,
            Ok(output) => return Response::invalid(output.text),
            Err(e) => return Response::invalid(e),
        };
        let files = staged.text.lines().filter(|l| !l.is_empty()).count();
        // A pending merge can change ancestry without changing the tree.
        // Git still validates the complete merge state when committing it.
        let merging = if files == 0 {
            match git(
                root.clone(),
                vec![
                    "rev-parse".into(),
                    "--verify".into(),
                    "--quiet".into(),
                    "MERGE_HEAD^{commit}".into(),
                ],
            )
            .await
            {
                Ok(output) => output.success,
                Err(error) => return Response::error(ErrorCode::Unavailable, error.to_string()),
            }
        } else {
            false
        };
        if files == 0 && !merging {
            return Response::error(
                ErrorCode::Conflict,
                if all {
                    "nothing to commit"
                } else {
                    "nothing staged; stage the changes or ask for --all"
                },
            );
        }

        // The parent, for the journal, and read before anything moves.
        let parent = match git(root.clone(), vec!["rev-parse".into(), "HEAD".into()]).await {
            Ok(output) if output.success => Some(output.text.trim().to_owned()),
            _ => None,
        };
        // From here the watcher must keep its hands off this checkout:
        // it polls on its own schedule and would otherwise see HEAD move
        // and write its own guessed-at entry for this very commit.
        //
        // A guard, not a pair of matching calls. Every way out of this
        // function from here — a failed commit, a failed rev-parse, a
        // future early return somebody adds, an unwind — has to clear
        // the mark, and a mark left behind does not fail loudly: it
        // silently stops that checkout being journaled for as long as
        // the daemon lives.
        let _committing = Committing::mark(self, root.clone());

        let mut args = vec!["commit".into()];
        if all {
            args.push("--all".into());
        }
        // `--message` and then the message as its own argument: a
        // message beginning with a dash is a message, not a flag.
        args.push("--message".into());
        args.push(message.clone());
        let made = git(root.clone(), args).await;
        let head = match made {
            Ok(output) if output.success => {
                match git(root.clone(), vec!["rev-parse".into(), "HEAD".into()]).await {
                    Ok(output) if output.success => Ok(output.text.trim().to_owned()),
                    Ok(output) => Err(output.text),
                    Err(e) => Err(e.to_string()),
                }
            }
            Ok(output) => Err(output.text),
            Err(e) => Err(e.to_string()),
        };
        let head = match head {
            Ok(head) => head,
            Err(reason) => return Response::invalid(reason),
        };
        // The full ref HEAD names, and the branch from it: never `--short`,
        // which git lengthens to `heads/<name>` once a tag shares the name.
        // A detached HEAD is not an error here; it is just no branch.
        let full_ref = {
            let dir = root.clone();
            tokio::task::spawn_blocking(move || super::authorship::head_ref(&dir))
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .filter(|r| r.starts_with("refs/heads/"))
        };
        let branch = full_ref
            .as_deref()
            .and_then(|r| r.strip_prefix("refs/heads/"))
            .map(str::to_owned);
        // The commit is the agent's as soon as git has made it, before any
        // push can hold it up: kept apart from the journal, so its word is
        // not taken on this branch however long the journal keeps it.
        // Recorded in a statement of its own: an `if let` scrutinee's
        // guard would live through the `return`, with `_committing` to
        // drop behind it.
        let recorded = lock(&self.state).record_daemon_commit(&agent, full_ref.as_deref(), &head);
        if let Some(error) = recorded {
            return error;
        }

        // Pushed after the commit exists, so a push that fails leaves a
        // commit rather than losing the work.
        let mut pushed = false;
        let mut trouble = None;
        if push {
            match git(root.clone(), vec!["push".into()]).await {
                Ok(output) if output.success => pushed = true,
                Ok(output) => trouble = Some(output.text),
                Err(e) => trouble = Some(e.to_string()),
            }
        }

        {
            let mut state = lock(&self.state);
            let short: String = head.chars().take(7).collect();
            let summary = format!("committed {short}: {}", first_line(&message));
            if let Some(record) = state.registry.get(&agent).cloned()
                && let Some(mut entry) = state.plain_entry(
                    &record,
                    JournalKind::Commit,
                    summary,
                    SummarySource::Explicit,
                )
            {
                entry.branch = branch.clone();
                entry.head_before = parent.clone();
                entry.head_after = Some(head.clone());
                entry.paths_total = files;
                state.append_journal(entry);
            }
            state.last_head.insert(root.clone(), head.clone());
            state.emit(EventKind::Committed {
                agent: agent.clone(),
                head: head.clone(),
                branch: branch.clone(),
                files,
                pushed,
            });
        }
        if let Some(reason) = trouble {
            // Not Internal: the commit was made and the state is
            // sound. What failed is a remote we do not control, which
            // is exactly what Unavailable is for — and the difference
            // matters to a caller deciding whether to retry.
            return Response::error(
                ErrorCode::Unavailable,
                format!("committed {head}, but the push failed: {reason}"),
            );
        }
        Response::Committed {
            head,
            branch,
            files,
            pushed,
        }
    }

    pub(super) async fn integrate(
        self: &Arc<Self>,
        reference: &str,
        source: String,
        validation: String,
        apply: bool,
    ) -> Response {
        let (agent, target, _) = match self.reader_checkout(reference) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        let source = match physical(PathBuf::from(source)).await {
            Ok(p) => p,
            Err(e) => return Response::invalid(e),
        };
        if source == target {
            return Response::invalid("source and target must be distinct checkouts");
        }
        let roots = (source.clone(), target.clone());
        let same_repository = tokio::task::spawn_blocking(move || {
            vcs::git_dirs(&roots.0)
                .zip(vcs::git_dirs(&roots.1))
                .is_some_and(|((_, a), (_, b))| project::canonical(&a) == project::canonical(&b))
        })
        .await
        .unwrap_or(false);
        if !same_repository {
            return Response::invalid(
                "integration requires linked worktrees of the same repository",
            );
        }
        let evidence = match lock(&self.state)
            .store
            .document::<Validation>("validation", &validation)
        {
            Ok(Some(v)) if v.passed() && v.checkout == source => v,
            Ok(None) => {
                return Response::error(ErrorCode::NotFound, "validation document not found");
            }
            Ok(Some(_)) => {
                return Response::error(
                    ErrorCode::Conflict,
                    "a passing validation from the source checkout is required",
                );
            }
            Err(e) => return Response::error(ErrorCode::StorageUnavailable, e.to_string()),
        };
        let target_environment = lock(&self.state)
            .registry
            .get(&agent)
            .and_then(agentdocker_core::container::ContainerEnvironment::of);
        if !agentdocker_core::container::ContainerEnvironment::matches(
            &evidence.environment,
            &target_environment,
        ) {
            return Response::error(
                ErrorCode::Conflict,
                "validation image environment differs from the integration target",
            );
        }
        for root in [&source, &target] {
            match git(
                root.clone(),
                vec![
                    "status".into(),
                    "--porcelain".into(),
                    "--untracked-files=all".into(),
                ],
            )
            .await
            {
                Ok(output) if output.success && output.text.is_empty() => {}
                _ => {
                    return Response::error(
                        ErrorCode::Conflict,
                        "both checkouts must be clean; commit source changes and validate the committed code first",
                    );
                }
            }
        }
        let root = source.clone();
        if !tokio::task::spawn_blocking(move || content::fingerprint(&root))
            .await
            .ok()
            .and_then(Result::ok)
            .is_some_and(|v| v == evidence.before)
        {
            return Response::error(
                ErrorCode::Conflict,
                "source content changed after validation",
            );
        }
        let root = source.clone();
        let source_state = tokio::task::spawn_blocking(move || vcs::state(&root))
            .await
            .ok()
            .flatten();
        let head = match source_state.and_then(|v| v.head) {
            Some(v) if Some(&v) == evidence.head.as_ref() => v,
            _ => {
                return Response::error(
                    ErrorCode::Conflict,
                    "source HEAD changed after validation",
                );
            }
        };
        if !apply {
            return match git(
                target,
                vec![
                    "diff".into(),
                    "--no-ext-diff".into(),
                    "--stat".into(),
                    format!("HEAD...{head}"),
                    "--".into(),
                ],
            )
            .await
            {
                Ok(output) if output.success => Response::Integration {
                    source_head: head,
                    applied: false,
                    clean: true,
                    text: output.text,
                },
                Ok(output) => Response::invalid(output.text),
                Err(e) => Response::invalid(e),
            };
        }
        // The integration lease remains until the caller reviews/commits and
        // explicitly releases it. A failed merge also retains this protection.
        match self
            .claim(
                reference,
                super::leases::ClaimArgs {
                    resource: format!("path:{}", target.display()),
                    mode: LeaseMode::Exclusive,
                    amount: None,
                    ttl_secs: 600,
                    note: Some(format!("integrating verified source {head}")),
                    wait_secs: 0,
                    priority: 0,
                    automatic: false,
                    fresh: false,
                    actor: None,
                },
            )
            .await
        {
            Response::Lease { .. } => {}
            other => return other,
        }
        // Verify target cleanliness again after acquiring the physical lease.
        match git(
            target.clone(),
            vec![
                "status".into(),
                "--porcelain".into(),
                "--untracked-files=all".into(),
            ],
        )
        .await
        {
            Ok(o) if o.success && o.text.is_empty() => {}
            _ => {
                return Response::error(
                    ErrorCode::Conflict,
                    "target changed before integration; lease retained for inspection",
                );
            }
        }
        match git(
            target,
            vec![
                "merge".into(),
                "--no-commit".into(),
                "--no-ff".into(),
                head.clone(),
            ],
        )
        .await
        {
            Ok(output) => {
                lock(&self.state).emit(EventKind::IntegrationPrepared {
                    agent,
                    source_head: head.clone(),
                    clean: output.success,
                });
                Response::Integration {
                    source_head: head,
                    applied: true,
                    clean: output.success,
                    text: output.text,
                }
            }
            Err(e) => {
                lock(&self.state).emit(EventKind::IntegrationPrepared {
                    agent,
                    source_head: head,
                    clean: false,
                });
                Response::invalid(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn commit_finishes_history_only_merges_but_refuses_ordinary_empty_commits() {
        async fn run(root: &Path, args: &[&str]) -> String {
            let result = git(
                root.to_owned(),
                args.iter().map(|s| (*s).to_owned()).collect(),
            )
            .await
            .unwrap();
            assert!(result.success, "git {args:?}: {}", result.text);
            result.text.trim().to_owned()
        }
        // Cover a normal checkout and a linked checkout with an octopus merge,
        // and both forms of the explicit commit command.
        for (all, sides) in [(false, 1), (true, 2)] {
            let temporary = tempfile::tempdir().unwrap();
            let repository = temporary.path().join("repository");
            std::fs::create_dir(&repository).unwrap();
            run(&repository, &["init", "-q", "-b", "main"]).await;
            run(
                &repository,
                &["config", "user.email", "test@example.invalid"],
            )
            .await;
            run(&repository, &["config", "user.name", "Fixture"]).await;
            std::fs::write(repository.join("file"), "unchanged").unwrap();
            run(&repository, &["add", "file"]).await;
            run(&repository, &["commit", "-qm", "initial"]).await;
            let parent = run(&repository, &["rev-parse", "HEAD"]).await;
            let tree = run(&repository, &["rev-parse", "HEAD^{tree}"]).await;
            let mut branches = Vec::new();
            for number in 0..sides {
                let branch = format!("side-{number}");
                run(&repository, &["checkout", "-qb", &branch, &parent]).await;
                run(&repository, &["commit", "--allow-empty", "-qm", &branch]).await;
                branches.push(branch);
            }
            run(&repository, &["checkout", "-q", "main"]).await;
            let root = if all {
                let linked = temporary.path().join("linked");
                run(
                    &repository,
                    &[
                        "worktree",
                        "add",
                        "-b",
                        "target",
                        linked.to_str().unwrap(),
                        "main",
                    ],
                )
                .await;
                linked
            } else {
                repository
            };
            std::fs::write(root.join("untracked"), "keep private").unwrap();
            let daemon = Arc::new(
                Daemon::open(
                    temporary.path().join("state"),
                    temporary.path().join("sock"),
                )
                .unwrap(),
            );
            daemon
                .handle(Request::Register {
                    spec: AgentSpec {
                        name: "merger".into(),
                        workdir: Some(root.clone()),
                        ..AgentSpec::default()
                    },
                    pid: None,
                    session: None,
                })
                .await;
            assert!(matches!(
                daemon.commit("merger", "empty".into(), all, false).await,
                Response::Error {
                    code: ErrorCode::Conflict,
                    ..
                }
            ));
            assert_eq!(run(&root, &["rev-parse", "HEAD"]).await, parent);
            let mut merge = vec!["merge", "--no-ff", "--no-commit"];
            merge.extend(branches.iter().map(String::as_str));
            run(&root, &merge).await;
            assert!(
                run(&root, &["diff", "--name-only", "--cached"])
                    .await
                    .is_empty()
            );
            let response = daemon
                .commit("merger", "merge ancestry".into(), all, false)
                .await;
            let Response::Committed {
                head,
                files: 0,
                pushed: false,
                ..
            } = response
            else {
                panic!("history-only merge refused: {response:?}");
            };
            assert_eq!(head, run(&root, &["rev-parse", "HEAD"]).await);
            assert_eq!(run(&root, &["rev-parse", "HEAD^{tree}"]).await, tree);
            assert_eq!(
                run(&root, &["rev-list", "--parents", "-n", "1", "HEAD"])
                    .await
                    .split_whitespace()
                    .count(),
                sides + 2
            );
            assert_eq!(
                std::fs::read_to_string(root.join("untracked")).unwrap(),
                "keep private"
            );
            assert_eq!(run(&root, &["status", "--porcelain"]).await, "?? untracked");
            assert!(matches!(
                daemon
                    .commit("merger", "still empty".into(), all, false)
                    .await,
                Response::Error {
                    code: ErrorCode::Conflict,
                    ..
                }
            ));
        }
    }
    #[tokio::test]
    async fn integration_requires_matching_validation_and_leaves_merge_uncommitted() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
        ] {
            assert!(
                git(root.clone(), args.into_iter().map(String::from).collect())
                    .await
                    .unwrap()
                    .success
            );
        }
        std::fs::write(root.join("file"), "one").unwrap();
        assert!(
            git(root.clone(), vec!["add".into(), "file".into()])
                .await
                .unwrap()
                .success
        );
        assert!(
            git(
                root.clone(),
                vec!["commit".into(), "-qm".into(), "initial".into()]
            )
            .await
            .unwrap()
            .success
        );
        let daemon =
            Arc::new(Daemon::open(tmp.path().join("state"), tmp.path().join("sock")).unwrap());
        daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "target".into(),
                    workdir: Some(root.clone()),
                    ..AgentSpec::default()
                },
                pid: None,
                session: None,
            })
            .await;
        let branch = tmp.path().join("branch");
        assert!(matches!(
            daemon
                .worktree_create(
                    "target",
                    Some(branch.to_string_lossy().into_owned()),
                    "feature".into(),
                    None,
                    Vec::new(),
                )
                .await,
            Response::Worktree { .. }
        ));
        daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "source".into(),
                    workdir: Some(branch.clone()),
                    ..AgentSpec::default()
                },
                pid: None,
                session: None,
            })
            .await;
        std::fs::write(branch.join("file"), "two").unwrap();
        assert!(
            git(
                branch.clone(),
                vec!["commit".into(), "-qam".into(), "change".into()]
            )
            .await
            .unwrap()
            .success
        );
        daemon.refresh_vcs(None).await;
        let Response::Validation {
            validation,
            passed: true,
        } = daemon
            .validate(
                "source",
                vec!["sh".into(), "-c".into(), "test -f file".into()],
                5,
            )
            .await
        else {
            panic!()
        };
        let source = branch.to_string_lossy().into_owned();
        assert!(matches!(
            daemon
                .integrate("target", source.clone(), "missing-validation".into(), false)
                .await,
            Response::Error {
                code: ErrorCode::NotFound,
                ..
            }
        ));
        assert!(matches!(
            daemon
                .integrate("target", source.clone(), validation.id.clone(), false)
                .await,
            Response::Integration { applied: false, .. }
        ));
        let mut image_evidence = validation.clone();
        image_evidence.environment = Some(agentdocker_core::container::ContainerEnvironment {
            inputs: None,
            image_id: "sha256:image".into(),
            build: "build".into(),
            engine: agentdocker_core::ContainerEngine::Docker,
            connection: None,
            network: Default::default(),
            user: None,
            env: Default::default(),
        });
        image_evidence.container = Some(agentdocker_core::recovery::ValidationContainer {
            agent: validation.agent.clone(),
            id: "container".into(),
        });
        assert!(image_evidence.passed());
        lock(&daemon.state)
            .store
            .put_document("validation", &validation.id, &image_evidence)
            .unwrap();
        assert!(
            matches!(daemon.integrate("target",source.clone(),validation.id.clone(),true).await,Response::Error {code:ErrorCode::Conflict,message,..} if message.contains("environment"))
        );
        assert!(!root.join(".git/MERGE_HEAD").exists());
        assert!(lock(&daemon.state).leases.is_empty());
        lock(&daemon.state)
            .store
            .put_document("validation", &validation.id, &validation)
            .unwrap();
        std::fs::write(branch.join("file"), "three").unwrap();
        assert!(matches!(
            daemon
                .integrate("target", source.clone(), validation.id.clone(), true)
                .await,
            Response::Error { .. }
        ));
        std::fs::write(branch.join("file"), "two").unwrap();
        // Git can ignore executable-bit changes while content fingerprints do
        // not. This reaches the content check with clean Git status and the
        // same HEAD, independently of the dirty-checkout guard.
        use std::os::unix::fs::PermissionsExt;
        assert!(
            git(
                branch.clone(),
                vec!["config".into(), "core.fileMode".into(), "false".into()]
            )
            .await
            .unwrap()
            .success
        );
        let permissions = std::fs::metadata(branch.join("file"))
            .unwrap()
            .permissions();
        std::fs::set_permissions(
            branch.join("file"),
            std::fs::Permissions::from_mode(permissions.mode() | 0o111),
        )
        .unwrap();
        assert!(
            git(branch.clone(), vec!["status".into(), "--porcelain".into()])
                .await
                .unwrap()
                .text
                .is_empty()
        );
        assert!(
            matches!(daemon.integrate("target", source.clone(), validation.id.clone(), true).await,
            Response::Error { code: ErrorCode::Conflict, message, .. } if message == "source content changed after validation")
        );
        std::fs::set_permissions(branch.join("file"), permissions).unwrap();
        assert!(matches!(
            daemon
                .integrate("target", source, validation.id, true)
                .await,
            Response::Integration {
                applied: true,
                clean: true,
                ..
            }
        ));
        assert_eq!(std::fs::read_to_string(root.join("file")).unwrap(), "two");
        assert!(
            root.join(".git/MERGE_HEAD").exists(),
            "review and commit remain explicit"
        );
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    async fn run(dir: &Path, args: &[&str]) {
        let output = git(
            dir.to_path_buf(),
            args.iter().map(|a| (*a).to_owned()).collect(),
        )
        .await
        .unwrap();
        assert!(output.success, "git {args:?}: {}", output.text);
    }

    #[tokio::test]
    async fn removal_reports_current_branch_and_full_tip_when_upstream_allows_squash_deletion() {
        async fn output(root: &Path, args: &[&str]) -> String {
            let result = git(root.to_owned(), args.iter().map(|a| (*a).into()).collect())
                .await
                .unwrap();
            assert!(result.success, "git {args:?}: {}", result.text);
            result.stdout.trim().to_owned()
        }
        for tracked in [false, true] {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path().join("repo");
            let worktree = temporary.path().join("worktree");
            let remote = temporary.path().join("remote.git");
            std::fs::create_dir(&root).unwrap();
            run(&root, &["init", "-q", "-b", "main"]).await;
            run(&root, &["config", "user.email", "fixture@example.invalid"]).await;
            run(&root, &["config", "user.name", "Fixture"]).await;
            run(&root, &["commit", "--allow-empty", "-qm", "initial"]).await;
            add_worktree(root.clone(), &worktree, "old-reviewed-branch", None, None)
                .await
                .unwrap();
            let reviewed_head = output(&worktree, &["rev-parse", "HEAD"]).await;
            // Both the branch and its tip change after an earlier UI review.
            run(&worktree, &["branch", "-m", "current-branch"]).await;
            std::fs::write(worktree.join("file"), "new work").unwrap();
            run(&worktree, &["add", "file"]).await;
            run(&worktree, &["commit", "-qm", "new tip"]).await;
            let current_head = output(&worktree, &["rev-parse", "HEAD"]).await;
            assert_ne!(reviewed_head, current_head);
            if tracked {
                run(&root, &["init", "--bare", "-q", remote.to_str().unwrap()]).await;
                run(
                    &root,
                    &["remote", "add", "origin", remote.to_str().unwrap()],
                )
                .await;
                run(&worktree, &["push", "-q", "-u", "origin", "current-branch"]).await;
            }
            run(&root, &["merge", "--squash", "current-branch"]).await;
            run(&root, &["commit", "-qm", "squashed work"]).await;
            assert_eq!(
                output(&root, &["rev-list", "--count", "main..current-branch"]).await,
                "1"
            );
            let (deleted, note) = remove_finished(root.clone(), worktree.clone())
                .await
                .unwrap();
            assert!(!worktree.exists());
            assert_eq!(
                deleted, tracked,
                "the same squash has different upstream outcomes"
            );
            let note = note.expect("Git outcome retained");
            assert!(note.contains("current-branch"), "{note}");
            assert!(!note.contains("old-reviewed-branch"));
            if deleted {
                assert!(
                    note.contains(&current_head),
                    "full deleted tip missing: {note}"
                );
                assert!(!note.contains(&reviewed_head));
                assert!(
                    output(&root, &["branch", "--list", "current-branch"])
                        .await
                        .is_empty()
                );
                // The reported full tip really reconstructs the removed branch.
                run(&root, &["branch", "--", "current-branch", &current_head]).await;
            } else {
                assert!(note.contains("kept"), "{note}");
            }
            assert_eq!(
                output(&root, &["rev-parse", "current-branch"]).await,
                current_head
            );
        }
    }

    /// A new worktree gets the ignored files `.worktreeinclude` names and
    /// nothing else; once its branch is merged and it is clean it can be
    /// removed with its branch, and a dirty one is refused.
    /// A checkout can carry a symlink where an include goes, or on its
    /// way: nothing is written through it, and what it points at is left
    /// as it was.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_include_is_never_written_through_a_symlink_in_the_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        run(&root, &["init", "-q", "-b", "main"]).await;
        run(&root, &["config", "user.email", "t@example.com"]).await;
        run(&root, &["config", "user.name", "T"]).await;
        std::fs::write(root.join(".gitignore"), ".env\nlocal/\n").unwrap();
        std::fs::write(root.join(INCLUDE_FILE), ".env\nlocal/\n").unwrap();
        run(&root, &["add", "."]).await;
        run(&root, &["commit", "-qm", "initial"]).await;
        std::fs::write(root.join(".env"), "KEY=secret\n").unwrap();
        std::fs::create_dir(root.join("local")).unwrap();
        std::fs::write(root.join("local/settings.json"), "{}").unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("victim"), "untouched").unwrap();

        let wt = tmp.path().join("wt");
        add_worktree(root.clone(), &wt, "agent/two", None, None)
            .await
            .unwrap();
        // What a checkout could have put there.
        std::os::unix::fs::symlink(outside.join("victim"), wt.join(".env")).unwrap();
        std::os::unix::fs::symlink(&outside, wt.join("local")).unwrap();
        let (copied, trouble) = copy_includes(root.clone(), wt.clone()).await;
        assert_eq!(copied, 0);
        assert!(trouble.unwrap().contains("symlink"));
        assert_eq!(
            std::fs::read_to_string(outside.join("victim")).unwrap(),
            "untouched"
        );
        assert!(!outside.join("settings.json").exists());
    }

    /// A repository with one commit, and a daemon with one agent working
    /// in it.
    async fn project_with_an_agent(tmp: &Path) -> (PathBuf, Arc<Daemon>) {
        let root = tmp.join("repo");
        std::fs::create_dir(&root).unwrap();
        run(&root, &["init", "-q", "-b", "main"]).await;
        run(&root, &["config", "user.email", "t@example.com"]).await;
        run(&root, &["config", "user.name", "T"]).await;
        run(&root, &["commit", "--allow-empty", "-qm", "initial"]).await;
        let daemon = Arc::new(Daemon::open(tmp.join("state"), tmp.join("sock")).unwrap());
        register(&daemon, "builder", &root).await;
        (project::canonical(&root), daemon)
    }

    async fn register(daemon: &Arc<Daemon>, name: &str, workdir: &Path) -> AgentId {
        let answer = daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: name.into(),
                    workdir: Some(workdir.to_path_buf()),
                    ..AgentSpec::default()
                },
                pid: None,
                session: None,
            })
            .await;
        match answer {
            Response::Agent { agent } => agent.id,
            other => panic!("{name}: {other:?}"),
        }
    }

    async fn create(
        daemon: &Arc<Daemon>,
        agent: &str,
        path: Option<&Path>,
        branch: &str,
    ) -> Response {
        daemon
            .handle(Request::WorktreeCreate {
                agent: agent.into(),
                path: path.map(|path| path.to_string_lossy().into_owned()),
                branch: branch.into(),
                from: None,
                seed_build: Vec::new(),
            })
            .await
    }

    fn made(answer: Response) -> PathBuf {
        match answer {
            Response::Worktree { path, .. } => path,
            other => panic!("no worktree: {other:?}"),
        }
    }

    fn refused(answer: &Response, why: &str) {
        assert!(
            matches!(answer, Response::Error { code: ErrorCode::Invalid, message, .. } if message.contains(why)),
            "expected a refusal saying `{why}`: {answer:?}"
        );
    }

    fn exclude_lines(root: &Path, line: &str) -> usize {
        std::fs::read_to_string(root.join(".git/info/exclude"))
            .unwrap_or_default()
            .lines()
            .filter(|known| known.trim() == line)
            .count()
    }

    /// A worktree asked for by branch alone goes in the project's own
    /// worktrees folder, which git is told once to leave alone, so the
    /// main checkout stays clean; a path of the caller's is still theirs
    /// to name, outside the checkout or in that folder, and a project
    /// whose worktrees all have one never gets the folder.
    #[tokio::test]
    async fn a_worktree_with_no_path_goes_in_the_projects_own_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, daemon) = project_with_an_agent(tmp.path()).await;
        let home = root.join(paths::PROJECT_WORKTREES);

        let outside = project::canonical(&tmp.path().join("outside"));
        assert_eq!(
            made(create(&daemon, "builder", Some(&outside), "agent/outside").await),
            outside
        );
        assert!(
            !root.join(".agentdocker").exists(),
            "a worktree with a path of its own makes no folder in the project"
        );

        let first = made(create(&daemon, "builder", None, "agent/first").await);
        assert_eq!(first, home.join("agent-first"));
        assert!(first.join(".git").is_file(), "a linked checkout");
        let second = made(create(&daemon, "builder", None, "second").await);
        assert_eq!(second, home.join("second"));
        assert_eq!(
            exclude_lines(&root, "/.agentdocker/worktrees/"),
            1,
            "git is told once"
        );
        let status = git(root.clone(), vec!["status".into(), "--porcelain".into()])
            .await
            .unwrap();
        assert_eq!(
            status.stdout.trim(),
            "",
            "the main checkout does not see the checkouts inside it"
        );

        // An agent working in one of them is in the same project: its
        // own worktree goes beside, not inside.
        register(&daemon, "nested", &first).await;
        assert_eq!(
            made(create(&daemon, "nested", None, "agent/third").await),
            home.join("agent-third")
        );
        refused(
            &create(&daemon, "nested", Some(&first.join("inner")), "agent/inner").await,
            "inside another checkout",
        );
        refused(
            &create(&daemon, "builder", Some(&second.join("a/b")), "agent/inner").await,
            "inside another checkout",
        );

        // Two branches whose names make one folder name: the second is
        // told, rather than given the first one's folder.
        refused(
            &create(&daemon, "builder", None, "agent-first").await,
            "is there already",
        );

        // Inside the project, the folder is the one place a path may name.
        refused(
            &create(
                &daemon,
                "builder",
                Some(&root.join("inside")),
                "agent/inside",
            )
            .await,
            "worktrees folder",
        );
        refused(
            &create(&daemon, "builder", Some(&home), "agent/home").await,
            "worktrees folder",
        );
        assert!(!root.join("inside").exists());
        assert_eq!(
            made(create(&daemon, "builder", Some(&home.join("mine")), "agent/mine").await),
            home.join("mine")
        );
    }

    /// A checkout someone else's commit made can hold a link where the
    /// worktrees folder goes. Nothing is made through it, and nothing
    /// where it points; a path of the caller's own is still theirs.
    #[cfg(unix)]
    #[tokio::test]
    async fn worktrees_are_not_made_through_a_link_in_the_project() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, daemon) = project_with_an_agent(tmp.path()).await;
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let untouched = || std::fs::read_dir(&elsewhere).unwrap().next().is_none();

        std::os::unix::fs::symlink(&elsewhere, root.join(".agentdocker")).unwrap();
        refused(
            &create(&daemon, "builder", None, "agent/first").await,
            "symbolic link",
        );
        assert!(untouched(), "nothing is made where the link points");

        std::fs::remove_file(root.join(".agentdocker")).unwrap();
        std::fs::create_dir(root.join(".agentdocker")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join(paths::PROJECT_WORKTREES)).unwrap();
        refused(
            &create(&daemon, "builder", None, "agent/first").await,
            "symbolic link",
        );
        assert!(untouched(), "nothing is made where the link points");
        assert_eq!(exclude_lines(&root, "/.agentdocker/worktrees/"), 0);

        let named = project::canonical(&tmp.path().join("named"));
        assert_eq!(
            made(create(&daemon, "builder", Some(&named), "agent/first").await),
            named
        );
    }

    /// The line in `info/exclude` is the least of git's ignore rules. A
    /// project whose own `.gitignore` keeps every folder in view defeats
    /// it: the worktree is refused with that rule named and the way
    /// around said, nothing is left behind, and the answer is the same
    /// once the line is there.
    #[tokio::test]
    async fn a_folder_the_projects_own_rules_keep_in_view_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, daemon) = project_with_an_agent(tmp.path()).await;
        std::fs::write(
            root.join(".gitignore"),
            "# every folder stays in view\n!*/\n",
        )
        .unwrap();
        run(&root, &["add", ".gitignore"]).await;
        run(&root, &["commit", "-qm", "rules"]).await;

        for round in ["the line is written", "the line is there"] {
            let answer = create(&daemon, "builder", None, "agent/first").await;
            refused(&answer, "`.gitignore:2:!*/`");
            refused(&answer, "[worktrees] dir");
            assert!(
                !root.join(".agentdocker").exists(),
                "{round}: what was made for it is taken away"
            );
            assert_eq!(
                exclude_lines(&root, "/.agentdocker/worktrees/"),
                1,
                "{round}"
            );
        }
        let listed = git(root.clone(), vec!["worktree".into(), "list".into()])
            .await
            .unwrap();
        assert_eq!(listed.stdout.lines().count(), 1, "{}", listed.stdout);
        let branches = git(
            root.clone(),
            vec!["branch".into(), "--list".into(), "agent/*".into()],
        )
        .await
        .unwrap();
        assert_eq!(branches.stdout.trim(), "", "no branch was made either");

        // A folder somebody else put there is not taken away with the refusal.
        std::fs::create_dir(root.join(".agentdocker")).unwrap();
        refused(
            &create(&daemon, "builder", None, "agent/first").await,
            ".gitignore:2:!*/",
        );
        assert!(root.join(".agentdocker").is_dir());
        assert!(!root.join(paths::PROJECT_WORKTREES).exists());

        // The ways around it: a folder outside the project, or a path.
        std::fs::write(
            daemon.home.join(agentdocker_core::config::FILE_NAME),
            format!(
                "[worktrees]\ndir = '{}'\n",
                tmp.path().join("elsewhere").display()
            ),
        )
        .unwrap();
        let first = made(create(&daemon, "builder", None, "agent/first").await);
        assert!(!first.starts_with(&root), "{}", first.display());
    }

    /// Whether the folder is ignored is git's answer and not a reading of
    /// its text: a rule that keeps the folder in view is found wherever
    /// it is written, a file whose path looks like git's own `file:line:`
    /// included, when the line is first written and when it is there.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_rule_that_keeps_the_folder_in_view_is_found_in_any_file() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, daemon) = project_with_an_agent(tmp.path()).await;
        std::fs::write(
            daemon.home.join(agentdocker_core::config::FILE_NAME),
            "[worktrees]\ndir = 'part:1:two/worktrees'\n",
        )
        .unwrap();
        std::fs::create_dir(root.join("part:1:two")).unwrap();
        std::fs::write(root.join("part:1:two/.gitignore"), "!worktrees/\n").unwrap();
        run(&root, &["add", "part:1:two/.gitignore"]).await;
        run(&root, &["commit", "-qm", "rules"]).await;

        for round in ["the line is written", "the line is there"] {
            let answer = create(&daemon, "builder", None, "agent/one").await;
            refused(&answer, "git does not ignore");
            refused(&answer, "`part:1:two/.gitignore:1:!worktrees/`");
            assert!(
                !root.join("part:1:two/worktrees").exists(),
                "{round}: what was made for it is taken away"
            );
            assert_eq!(exclude_lines(&root, "/part:1:two/worktrees/"), 1, "{round}");
        }
        let untracked = git(
            root.clone(),
            vec![
                "status".into(),
                "--porcelain".into(),
                "--untracked-files=all".into(),
            ],
        )
        .await
        .unwrap();
        assert_eq!(untracked.stdout.trim(), "", "no checkout was made");
        let listed = git(root.clone(), vec!["worktree".into(), "list".into()])
            .await
            .unwrap();
        assert_eq!(listed.stdout.lines().count(), 1, "{}", listed.stdout);

        // Without the rule the same folder is taken.
        std::fs::write(root.join("part:1:two/.gitignore"), "").unwrap();
        assert_eq!(
            made(create(&daemon, "builder", None, "agent/one").await),
            root.join("part:1:two/worktrees/agent-one")
        );
    }

    /// A folder name is taken to the letter in the line that keeps it out
    /// of git. What git would read as a pattern is escaped, so the folder
    /// is ignored and a folder the pattern would have matched is not; a
    /// name no line can hold is refused.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_folder_name_git_would_read_as_a_pattern_is_named_to_the_letter() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, daemon) = project_with_an_agent(tmp.path()).await;
        let settings = daemon.home.join(agentdocker_core::config::FILE_NAME);
        // What the unescaped lines would have matched as well.
        for sibling in ["checkXoutsl1", "backslash"] {
            std::fs::create_dir(root.join(sibling)).unwrap();
            std::fs::write(root.join(sibling).join("seen.txt"), "seen").unwrap();
        }
        let untracked = async || {
            git(
                root.clone(),
                vec![
                    "status".into(),
                    "--porcelain".into(),
                    "--untracked-files=all".into(),
                ],
            )
            .await
            .unwrap()
            .stdout
        };
        let siblings = "?? backslash/seen.txt\n?? checkXoutsl1/seen.txt\n";

        std::fs::write(&settings, "[worktrees]\ndir = 'check*outs[local]?'\n").unwrap();
        assert_eq!(
            made(create(&daemon, "builder", None, "agent/first").await),
            root.join("check*outs[local]?/agent-first")
        );
        assert_eq!(exclude_lines(&root, r"/check\*outs\[local\]\?/"), 1);
        assert_eq!(untracked().await, siblings);

        std::fs::write(&settings, "[worktrees]\ndir = 'back\\slash'\n").unwrap();
        assert_eq!(
            made(create(&daemon, "builder", None, "agent/second").await),
            root.join("back\\slash/agent-second")
        );
        assert_eq!(exclude_lines(&root, r"/back\\slash/"), 1);
        assert_eq!(untracked().await, siblings);

        std::fs::write(&settings, "[worktrees]\ndir = \"two\\nlines\"\n").unwrap();
        refused(
            &create(&daemon, "builder", None, "agent/third").await,
            "no git ignore rule can hold",
        );
        assert_eq!(untracked().await, siblings);
    }

    /// The line itself: one folder, rooted at the project's top, with
    /// what git reads as a pattern escaped; none for a folder outside the
    /// project, and a reason for a name a line cannot hold.
    #[cfg(unix)]
    #[test]
    fn the_exclude_line_names_one_folder_to_the_letter() {
        let root = Path::new("/work/app");
        let line = |folder: &str| exclude_line(root, &root.join(folder));
        assert_eq!(
            line(".agentdocker/worktrees"),
            Ok(Some("/.agentdocker/worktrees/".to_owned()))
        );
        assert_eq!(
            line("a*b/c[d]?\\e f"),
            Ok(Some(r"/a\*b/c\[d\]\?\\e f/".to_owned()))
        );
        assert_eq!(exclude_line(root, Path::new("/elsewhere/trees")), Ok(None));
        assert_eq!(exclude_line(root, root), Ok(None));
        for unnamable in ["two\nlines", "tab\there", "bell\u{7}"] {
            assert!(line(unnamable).is_err(), "{unnamable:?}");
        }
    }

    async fn remove(
        daemon: &Arc<Daemon>,
        agent: &str,
        path: Option<&Path>,
        branch: Option<&str>,
    ) -> Response {
        daemon
            .handle(Request::WorktreeRemoveNamed {
                agent: agent.into(),
                path: path.map(|path| path.to_string_lossy().into_owned()),
                branch: branch.map(str::to_owned),
            })
            .await
    }

    fn answered(answer: &Response, code: ErrorCode, why: &str) {
        assert!(
            matches!(answer, Response::Error { code: said, message, .. } if *said == code && message.contains(why)),
            "expected {code:?} saying `{why}`: {answer:?}"
        );
    }

    async fn request(daemon: &Arc<Daemon>, request: serde_json::Value) -> Response {
        daemon
            .handle(serde_json::from_value(request).expect("a request"))
            .await
    }

    /// A running agent removes a worktree of its project that nobody
    /// works in, by its path or its branch, whichever agent made it. The
    /// main folder, the asker's own checkout, one another agent works in
    /// or holds a path in, and one with unsaved work all stay, each with
    /// its reason; a branch git does not consider merged is kept when its
    /// worktree goes.
    #[tokio::test]
    async fn an_agent_removes_a_worktree_nobody_works_in() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, daemon) = project_with_an_agent(tmp.path()).await;
        let first = made(create(&daemon, "builder", None, "agent/first").await);
        let second = made(create(&daemon, "builder", None, "agent/second").await);

        // Named by exactly one of the two, and a worktree of this project.
        refused(
            &remove(&daemon, "builder", None, None).await,
            "one of the two",
        );
        refused(
            &remove(&daemon, "builder", Some(&first), Some("agent/first")).await,
            "one of the two",
        );
        answered(
            &remove(&daemon, "builder", None, Some("agent/none")).await,
            ErrorCode::NotFound,
            "agent/none",
        );
        answered(
            &remove(&daemon, "builder", Some(&tmp.path().join("nowhere")), None).await,
            ErrorCode::NotFound,
            "nowhere",
        );
        refused(
            &remove(&daemon, "builder", Some(&root), None).await,
            "main folder",
        );
        refused(
            &remove(&daemon, "builder", None, Some("main")).await,
            "main folder",
        );

        // Nobody is removed from under: not another agent, not the asker.
        let nested = register(&daemon, "nested", &first).await;
        answered(
            &remove(&daemon, "builder", Some(&first), None).await,
            ErrorCode::Conflict,
            "nested",
        );
        answered(
            &remove(&daemon, "nested", None, Some("agent/first")).await,
            ErrorCode::Conflict,
            "you are working in that worktree",
        );

        // Unsaved work stays where it is.
        std::fs::write(second.join("note.txt"), "unsaved").unwrap();
        answered(
            &remove(&daemon, "builder", Some(&second), None).await,
            ErrorCode::Conflict,
            "uncommitted or untracked",
        );
        std::fs::remove_file(second.join("note.txt")).unwrap();

        // A path held there keeps it, whoever holds it: the holder
        // releases it first.
        let held = format!("path:{}", second.join("held.rs").display());
        let claimed = request(
            &daemon,
            serde_json::json!({"op": "claim", "agent": "nested", "resource": held}),
        )
        .await;
        assert!(!matches!(claimed, Response::Error { .. }), "{claimed:?}");
        answered(
            &remove(&daemon, "builder", Some(&second), None).await,
            ErrorCode::Conflict,
            "nested holds",
        );
        answered(
            &remove(&daemon, "nested", None, Some("agent/second")).await,
            ErrorCode::Conflict,
            "you hold",
        );
        assert!(second.exists());
        let released = request(
            &daemon,
            serde_json::json!({"op": "release_resource", "agent": "nested", "resource": held}),
        )
        .await;
        assert!(!matches!(released, Response::Error { .. }), "{released:?}");
        // And so does its branch, held.
        let claimed = request(
            &daemon,
            serde_json::json!({"op": "claim", "agent": "nested", "resource": "branch:agent/second"}),
        )
        .await;
        assert!(!matches!(claimed, Response::Error { .. }), "{claimed:?}");
        answered(
            &remove(&daemon, "builder", Some(&second), None).await,
            ErrorCode::Conflict,
            "nested holds branch:agent/second",
        );
        let released = request(
            &daemon,
            serde_json::json!({"op": "release_resource", "agent": "nested", "resource": "branch:agent/second"}),
        )
        .await;
        assert!(!matches!(released, Response::Error { .. }), "{released:?}");

        // The agent that did not make it removes it, by its branch.
        let answer = remove(&daemon, "nested", None, Some("agent/second")).await;
        let Response::WorktreeRemoved {
            path,
            branch_removed,
            ..
        } = &answer
        else {
            panic!("not removed: {answer:?}");
        };
        assert_eq!(*path, second);
        assert!(
            *branch_removed,
            "a branch with nothing of its own is merged"
        );
        assert!(!second.exists());
        assert!(
            daemon.recent_events(50).iter().any(|event| matches!(
                &event.kind,
                EventKind::WorktreeRemoved { by, path, branch, branch_removed: true, .. }
                    if *by == nested && *path == second && branch.as_deref() == Some("agent/second")
            )),
            "the removal is announced, with who asked"
        );
        let listed = git(
            first.clone(),
            vec!["worktree".into(), "list".into(), "--porcelain".into()],
        )
        .await
        .unwrap();
        assert!(!listed.stdout.contains("agent-second"), "{}", listed.stdout);

        // A branch with a commit of its own is kept when its worktree goes.
        let third = made(create(&daemon, "builder", None, "agent/third").await);
        std::fs::write(third.join("work.txt"), "work").unwrap();
        run(&third, &["add", "work.txt"]).await;
        run(&third, &["commit", "-qm", "work"]).await;
        let answer = remove(&daemon, "builder", Some(&third), None).await;
        assert!(
            matches!(&answer, Response::WorktreeRemoved { branch_removed: false, note: Some(note), .. } if note.contains("is kept")),
            "{answer:?}"
        );
        assert!(!third.exists());
        let branches = git(
            root.clone(),
            vec!["branch".into(), "--list".into(), "agent/third".into()],
        )
        .await
        .unwrap();
        assert!(
            branches.stdout.contains("agent/third"),
            "{}",
            branches.stdout
        );
    }

    /// Git takes several commands to remove a worktree, and the daemon
    /// does not hold its state while they run. The look that finds the
    /// worktree idle reserves it in the same step: with the removal
    /// stopped between that look and git, nobody registers there and
    /// nothing in it, over it or on its branch is claimed, by anybody;
    /// the rest of the project is not held up; and the reservation ends
    /// with the removal, whether it removed the worktree or refused to.
    #[tokio::test]
    async fn nobody_enters_a_worktree_while_it_is_being_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, daemon) = project_with_an_agent(tmp.path()).await;
        register(&daemon, "bystander", &tmp.path().join("repo")).await;
        let target = made(create(&daemon, "builder", None, "agent/going").await);
        let enter = async |name: &str, workdir: &Path| {
            daemon
                .handle(Request::Register {
                    spec: AgentSpec {
                        name: name.into(),
                        workdir: Some(workdir.to_path_buf()),
                        ..AgentSpec::default()
                    },
                    pid: None,
                    session: None,
                })
                .await
        };
        let claim = async |who: &str, resource: &str| {
            request(
                &daemon,
                serde_json::json!({"op": "claim", "agent": who, "resource": resource}),
            )
            .await
        };

        let hold = Arc::new(tokio::sync::Barrier::new(2));
        *lock(&daemon.removal_hold) = Some(hold.clone());
        let removal = tokio::spawn({
            let (daemon, target) = (daemon.clone(), target.clone());
            async move { remove(&daemon, "builder", Some(&target), None).await }
        });
        // The removal has found the worktree idle and reserved it; git has
        // not been asked yet.
        hold.wait().await;
        assert!(target.exists());
        answered(
            &enter("late", &target).await,
            ErrorCode::Conflict,
            "is being removed",
        );
        let inside = format!("path:{}", target.join("src/lib.rs").display());
        let over = format!("path:{}", target.parent().unwrap().display());
        for resource in [inside.as_str(), over.as_str(), "branch:agent/going"] {
            for who in ["bystander", "builder"] {
                answered(
                    &claim(who, resource).await,
                    ErrorCode::Conflict,
                    "is being removed",
                );
            }
        }
        answered(
            &remove(&daemon, "bystander", Some(&target), None).await,
            ErrorCode::Conflict,
            "being removed already",
        );
        // The rest of the project is not held up.
        let elsewhere = format!("path:{}", root.join("src/lib.rs").display());
        let granted = claim("bystander", &elsewhere).await;
        assert!(!matches!(granted, Response::Error { .. }), "{granted:?}");

        hold.wait().await;
        let answer = removal.await.unwrap();
        assert!(
            matches!(&answer, Response::WorktreeRemoved { path, .. } if *path == target),
            "{answer:?}"
        );
        assert!(!target.exists());
        // Its reservation went with it.
        let granted = claim("bystander", "branch:agent/going").await;
        assert!(!matches!(granted, Response::Error { .. }), "{granted:?}");

        // A removal that is refused lets go as well.
        let kept = made(create(&daemon, "builder", None, "agent/kept").await);
        std::fs::write(kept.join("note.txt"), "unsaved").unwrap();
        answered(
            &remove(&daemon, "builder", Some(&kept), None).await,
            ErrorCode::Conflict,
            "uncommitted or untracked",
        );
        let granted = claim(
            "bystander",
            &format!("path:{}", kept.join("src/lib.rs").display()),
        )
        .await;
        assert!(!matches!(granted, Response::Error { .. }), "{granted:?}");
        let entered = enter("late", &kept).await;
        assert!(matches!(entered, Response::Agent { .. }), "{entered:?}");
    }

    /// `git worktree list --porcelain -z` as the removal reads it: each
    /// checkout's place and branch, the main one first, a detached one
    /// with no branch.
    #[test]
    fn the_worktree_listing_is_read_by_place_and_branch() {
        let text = "worktree /work/app\0HEAD 1111\0branch refs/heads/main\0\0\
                    worktree /work/app/.agentdocker/worktrees/agent-fix\0HEAD 2222\0branch refs/heads/agent/fix\0\0\
                    worktree /work/detached\0HEAD 3333\0detached\0\0";
        let all = listed(text);
        let places: Vec<(&Path, Option<&str>)> = all
            .iter()
            .map(|one| (one.path.as_path(), one.branch.as_deref()))
            .collect();
        assert_eq!(
            places,
            [
                (Path::new("/work/app"), Some("main")),
                (
                    Path::new("/work/app/.agentdocker/worktrees/agent-fix"),
                    Some("agent/fix")
                ),
                (Path::new("/work/detached"), None),
            ]
        );
    }

    /// `[worktrees] dir` in the daemon's settings moves the folder: an
    /// absolute one holds a folder per project and leaves the project
    /// untouched, a relative one is inside the project and kept out of
    /// git like the default, and one that names no usable place refuses a
    /// worktree that has no path of its own, with the reason.
    #[tokio::test]
    async fn the_daemons_settings_say_where_worktrees_go() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, daemon) = project_with_an_agent(tmp.path()).await;
        let settings = daemon.home.join(agentdocker_core::config::FILE_NAME);

        let elsewhere = tmp.path().join("elsewhere");
        std::fs::write(
            &settings,
            format!("[worktrees]\ndir = '{}'\n", elsewhere.display()),
        )
        .unwrap();
        let first = made(create(&daemon, "builder", None, "agent/first").await);
        let elsewhere = project::canonical(&elsewhere);
        assert_eq!(first.file_name().unwrap(), "agent-first");
        assert_eq!(
            first.parent().and_then(Path::parent),
            Some(elsewhere.as_path()),
            "a folder per project under the one the settings name: {}",
            first.display()
        );
        assert!(!root.join(".agentdocker").exists());
        assert_eq!(exclude_lines(&root, "/.agentdocker/worktrees/"), 0);

        std::fs::write(&settings, "[worktrees]\ndir = \".trees\"\n").unwrap();
        assert_eq!(
            made(create(&daemon, "builder", None, "agent/second").await),
            root.join(".trees/agent-second")
        );
        assert_eq!(exclude_lines(&root, "/.trees/"), 1);

        std::fs::write(&settings, "[worktrees]\ndir = \"../beside\"\n").unwrap();
        refused(&create(&daemon, "builder", None, "agent/third").await, "..");
        let named = project::canonical(&tmp.path().join("named"));
        assert_eq!(
            made(create(&daemon, "builder", Some(&named), "agent/third").await),
            named,
            "a path of the caller's own does not need the folder"
        );
    }

    /// `seed_build` brings the asking agent's build output into the new
    /// worktree and says how; a missing one is skipped, and a bad name is
    /// refused before anything is made.
    #[tokio::test]
    async fn a_new_worktree_is_seeded_with_build_output_from_the_agents_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        run(&root, &["init", "-q", "-b", "main"]).await;
        run(&root, &["config", "user.email", "t@example.com"]).await;
        run(&root, &["config", "user.name", "T"]).await;
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        run(&root, &["add", "."]).await;
        run(&root, &["commit", "-qm", "initial"]).await;
        std::fs::create_dir_all(root.join("target/debug/deps")).unwrap();
        std::fs::write(root.join("target/debug/deps/libdep.rlib"), "built").unwrap();
        let daemon =
            Arc::new(Daemon::open(tmp.path().join("state"), tmp.path().join("sock")).unwrap());
        daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "builder".into(),
                    workdir: Some(root.clone()),
                    ..AgentSpec::default()
                },
                pid: None,
                session: None,
            })
            .await;

        let refused = tmp.path().join("refused");
        let answer = daemon
            .handle(Request::WorktreeCreate {
                agent: "builder".into(),
                path: Some(refused.to_string_lossy().into_owned()),
                branch: "agent/refused".into(),
                from: None,
                seed_build: vec!["../outside".into()],
            })
            .await;
        assert!(
            matches!(&answer, Response::Error { message, .. } if message.contains("..")),
            "{answer:?}"
        );
        assert!(!refused.exists(), "nothing is made for a bad name");

        let wt = tmp.path().join("wt");
        let Response::Worktree { path, seeded, .. } = daemon
            .handle(Request::WorktreeCreate {
                agent: "builder".into(),
                path: Some(wt.to_string_lossy().into_owned()),
                branch: "agent/seeded".into(),
                from: None,
                seed_build: vec!["target".into(), "node_modules".into()],
            })
            .await
        else {
            panic!("worktree_create failed");
        };
        assert_eq!(seeded.len(), 2, "{seeded:?}");
        assert_eq!(seeded[0].dir, "target");
        if cfg!(unix) {
            assert!(seeded[0].outcome.seeded(), "{seeded:?}");
            assert_eq!(
                std::fs::read_to_string(path.join("target/debug/deps/libdep.rlib")).unwrap(),
                "built"
            );
        } else {
            // Nothing is seeded on Windows yet, and the answer says so.
            assert_eq!(
                seeded[0].outcome,
                agentdocker_core::seed::Outcome::Refused,
                "{seeded:?}"
            );
            assert!(!path.join("target").exists());
        }
        // A missing one is skipped where seeding is done at all; Windows
        // refuses every name alike.
        assert_eq!(
            seeded[1].outcome,
            if cfg!(unix) {
                agentdocker_core::seed::Outcome::Skipped
            } else {
                agentdocker_core::seed::Outcome::Refused
            },
            "{seeded:?}"
        );
        // Seeded output is ignored, so the new checkout is clean.
        let status = git(path.clone(), vec!["status".into(), "--porcelain".into()])
            .await
            .unwrap();
        assert!(
            status.success && status.stdout.trim().is_empty(),
            "{}",
            status.text
        );
    }

    #[tokio::test]
    async fn includes_are_copied_in_and_a_finished_worktree_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        run(&root, &["init", "-q", "-b", "main"]).await;
        run(&root, &["config", "user.email", "t@example.com"]).await;
        run(&root, &["config", "user.name", "T"]).await;
        std::fs::write(root.join(".gitignore"), ".env\nlocal/\n").unwrap();
        std::fs::write(
            root.join(INCLUDE_FILE),
            "# secrets the agent needs\n.env\nlocal/\n",
        )
        .unwrap();
        std::fs::write(root.join("README.md"), "hello\n").unwrap();
        run(&root, &["add", "."]).await;
        run(&root, &["commit", "-qm", "initial"]).await;
        std::fs::write(root.join(".env"), "KEY=1\n").unwrap();
        std::fs::create_dir(root.join("local")).unwrap();
        std::fs::write(root.join("local/settings.json"), "{}").unwrap();
        std::fs::write(root.join("scratch.txt"), "not listed").unwrap();

        let wt = tmp.path().join("wt");
        add_worktree(root.clone(), &wt, "agent/one", None, None)
            .await
            .unwrap();
        let (copied, trouble) = copy_includes(root.clone(), wt.clone()).await;
        if cfg!(unix) {
            assert_eq!((copied, trouble), (2, None));
            assert_eq!(std::fs::read_to_string(wt.join(".env")).unwrap(), "KEY=1\n");
            assert!(wt.join("local/settings.json").exists());
            assert!(!wt.join("scratch.txt").exists(), "only what is listed");
        } else {
            // Nothing copied unsafely, and the worktree says why.
            assert_eq!(copied, 0);
            assert!(trouble.unwrap().contains("not copied on this platform"));
            assert!(!wt.join(".env").exists());
        }

        // Dirty: refused. Committed and merged: removed with its branch.
        std::fs::write(wt.join("new.rs"), "fn x() {}\n").unwrap();
        assert!(remove_finished(root.clone(), wt.clone()).await.is_err());
        run(&wt, &["add", "new.rs"]).await;
        run(&wt, &["commit", "-qm", "work"]).await;
        // The include files are ignored, so the worktree is clean; the
        // branch is not merged yet, so it is kept.
        let (branch_removed, note) = remove_finished(root.clone(), wt.clone()).await.unwrap();
        assert!(!branch_removed);
        assert!(note.unwrap().contains("kept"));
        assert!(!wt.exists());
        run(&root, &["merge", "-q", "--no-ff", "-m", "m", "agent/one"]).await;
        run(&root, &["branch", "-d", "agent/one"]).await;
    }
}
