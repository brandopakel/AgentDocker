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
async fn physical(raw: String) -> anyhow::Result<PathBuf> {
    tokio::task::spawn_blocking(move || project::try_canonical(Path::new(&raw)))
        .await?
        .map_err(Into::into)
}
fn failure(e: impl std::fmt::Display) -> Response {
    Response::error(ErrorCode::Invalid, e.to_string())
}

/// Holds a checkout marked as "the daemon is committing here" for as
/// long as it lives, and lets go however the commit ends.
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

/// The subject of a commit message: what a journal line can carry.
fn first_line(message: &str) -> &str {
    message.lines().next().unwrap_or("").trim()
}

/// `git worktree add -b <branch> <path> HEAD` in `root`, for a new path
/// outside the checkout and a branch name git accepts. Shared by
/// `worktree-create` and `run --isolate`.
pub(super) async fn add_worktree(
    root: PathBuf,
    path: &Path,
    branch: &str,
    from: Option<&str>,
) -> Result<(), Box<Response>> {
    if path.exists() || path.starts_with(&root) {
        return Err(Box::new(failure(
            "worktree path must be new and outside the current checkout",
        )));
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
        _ => return Err(Box::new(failure("invalid branch name"))),
    }
    // The start point is a ref, never an option: `--` ends the flags.
    let start = match from {
        Some(from) if from.starts_with('-') || from.trim().is_empty() => {
            return Err(Box::new(failure("invalid start point")));
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
        Ok(output) => Err(Box::new(failure(output.text))),
        Err(e) => Err(Box::new(failure(e))),
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

impl Daemon {
    /// A linked checkout on a new branch, from this session's HEAD or
    /// from `from` (a branch, tag or commit of the same repository). The
    /// daemon remembers who asked, so commits made there with git alone
    /// are still that agent's in the journal.
    ///
    /// `seed_build` names directories of the agent's checkout (build
    /// output such as `target` or `node_modules`) to clone or copy into
    /// the new one, once it exists and off the state lock; what became of
    /// each is in the answer, and none of it fails the worktree.
    pub(super) async fn worktree_create(
        &self,
        reference: &str,
        path: String,
        branch: String,
        from: Option<String>,
        seed_build: Vec<String>,
    ) -> Response {
        // A bad name is the caller's to fix before anything is made.
        if let Err(e) = agentdocker_core::seed::check(&seed_build) {
            return failure(e);
        }
        let (agent, root, _) = match self.reader_checkout(reference) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        let path = match physical(path).await {
            Ok(p) => p,
            Err(e) => return failure(e),
        };
        if let Err(response) = add_worktree(root.clone(), &path, &branch, from.as_deref()).await {
            return *response;
        }
        {
            let mut state = lock(&self.state);
            state.worktree_creators.insert(path.clone(), agent.clone());
            state.emit(EventKind::WorktreeCreated {
                agent,
                path: path.clone(),
            });
        }
        let seeded = seed_build_dirs(root, path.clone(), seed_build).await;
        Response::Worktree {
            path,
            branch,
            seeded,
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
            Ok(output) => failure(output.text),
            Err(e) => failure(e),
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
            return failure("a commit needs a message");
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
            Ok(output) => return failure(output.text),
            Err(e) => return failure(e),
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
            Err(reason) => return failure(reason),
        };
        let branch = match git(
            root.clone(),
            vec![
                "symbolic-ref".into(),
                "--short".into(),
                "--quiet".into(),
                "HEAD".into(),
            ],
        )
        .await
        {
            // A detached HEAD is not an error here; it is just no branch.
            Ok(output) if output.success => Some(output.text.trim().to_owned()),
            _ => None,
        };

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
        let source = match physical(source).await {
            Ok(p) => p,
            Err(e) => return failure(e),
        };
        if source == target {
            return failure("source and target must be distinct checkouts");
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
            return failure("integration requires linked worktrees of the same repository");
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
                Ok(output) => failure(output.text),
                Err(e) => failure(e),
            };
        }
        // The integration lease remains until the caller reviews/commits and
        // explicitly releases it. A failed merge also retains this protection.
        match self
            .claim(
                reference,
                format!("path:{}", target.display()),
                LeaseMode::Exclusive,
                None,
                600,
                Some(format!("integrating verified source {head}")),
                0,
                false,
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
                failure(e)
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
                    branch.to_string_lossy().into_owned(),
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
            add_worktree(root.clone(), &worktree, "old-reviewed-branch", None)
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
        add_worktree(root.clone(), &wt, "agent/two", None)
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
                path: refused.to_string_lossy().into_owned(),
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
                path: wt.to_string_lossy().into_owned(),
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
        assert_eq!(
            seeded[1].outcome,
            agentdocker_core::seed::Outcome::Skipped,
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
        add_worktree(root.clone(), &wt, "agent/one", None)
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
