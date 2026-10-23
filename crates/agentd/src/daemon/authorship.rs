//! Who made which commits, as the daemon itself made or saw them, kept
//! apart from the journal (which pruning and retention may empty) so that
//! no agent's word is taken on a branch it wrote, wherever its checkout
//! stands later (`code_verdict`).
//!
//! Two witnesses, and nothing reconstructed afterwards: a commit the daemon
//! makes for an agent (`commit`), and a commit made in a worktree the
//! daemon created, reported by the post-commit hook it installed for it.
//! A report carries the worktree's witness token and names HEAD and the
//! full ref HEAD is on, both checked against git; it is recorded as the
//! commit of whichever of the worktree's owners — a live agent holding an
//! exclusive `path:` lease over the whole worktree, else the live agent it
//! was created for — is the session it was sent from. A detached HEAD
//! names no branch and records nothing; a checkout or a fast-forward runs
//! no post-commit hook and records nothing. Commits made any other way
//! (git in a checkout the daemon did not create, a merge commit, which git
//! makes without running post-commit) are not witnessed: only the
//! verdict's current-checkout refusal covers them.
use super::*;
use crate::store::authorship::WitnessedWorktree;
use agentdocker_core::gate::{self, Authorship, WITNESS_FILE, Witness, WitnessFile};
use agentdocker_core::secret::SecretText;
use sha2::{Digest, Sha256};

/// The line that marks a post-commit hook as AgentDocker's.
const HOOK_MARK: &str = "# agentdocker: witness hook";
/// What a post-commit hook that was already there is renamed to; ours runs
/// it after reporting.
pub(super) const CHAINED: &str = "post-commit.agentdocker-chained";
/// A hook already there is read whole, up to this, to tell whether it is
/// ours; a larger one is left alone.
const HOOK_BYTES: u64 = 1024 * 1024;
/// The file in the hooks folder installers lock in turn, across threads
/// and processes, for the whole look, move and write.
const HOOK_LOCK: &str = "post-commit.agentdocker-lock";
/// How long an installer waits for another to finish with the folder.
const HOOK_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

fn hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

/// `text` as one word for `sh`.
fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// The post-commit hook: in a worktree whose own git directory holds a
/// witness file it reports the commit just made, with the `agentdocker`
/// command at `cli` (or on the PATH once that is gone), never starting a
/// daemon and never failing the commit; then, anywhere, it runs the
/// post-commit hook that was there before it, if there was one.
pub(super) fn hook_script(cli: &Path) -> String {
    let cli = cli.to_string_lossy();
    // Git for Windows runs hooks with its own sh, which takes `/`.
    let cli = if cfg!(windows) {
        cli.replace('\\', "/")
    } else {
        cli.into_owned()
    };
    format!(
        "#!/bin/sh\n\
         {HOOK_MARK}\n\
         # AgentDocker installed this. In a worktree it created it reports the\n\
         # commit just made, so the daemon knows which agent made it; then it\n\
         # runs the post-commit hook that was here before it, if there was one.\n\
         witness=\"$(git rev-parse --absolute-git-dir 2>/dev/null)/{WITNESS_FILE}\"\n\
         if [ -f \"$witness\" ]; then\n\
         \tcli={cli}\n\
         \t[ -x \"$cli\" ] || cli=agentdocker\n\
         \tAGENTDOCKER_NO_AUTOSTART=1 \"$cli\" report-commit --witness \"$witness\" </dev/null >/dev/null 2>&1 || :\n\
         fi\n\
         chained=\"${{0%/*}}/{CHAINED}\"\n\
         # Never one of ours under that name: it would run itself for ever.\n\
         if [ -x \"$chained\" ] && ! grep -qF '{HOOK_MARK}' \"$chained\" 2>/dev/null; then\n\
         \texec \"$chained\" \"$@\"\n\
         fi\n\
         exit 0\n",
        cli = sh_quote(&cli),
    )
}

/// Write `text` to `path` as a new executable file, fsynced. The path
/// must name nothing yet: an existing file or symlink there is never
/// opened, let alone followed (`O_EXCL`, and `O_NOFOLLOW` where there is
/// one). What was created is removed again when the write fails.
fn write_executable(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o755).custom_flags(nix::libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all());
    if written.is_err() {
        drop(file);
        let _ = std::fs::remove_file(path);
    }
    written
}

/// Replace `hook` whole with `text`: written to a new file under a name
/// nobody can have put anything at beforehand, then renamed over it, so a
/// commit running the hook meanwhile reads one or the other. The new file
/// is removed again when anything fails.
fn replace_executable(hooks: &Path, hook: &Path, text: &str) -> std::io::Result<()> {
    let fresh = hooks.join(format!(
        "post-commit.agentdocker-new.{}",
        uuid::Uuid::new_v4().simple()
    ));
    write_executable(&fresh, text)?;
    std::fs::rename(&fresh, hook).inspect_err(|_| {
        let _ = std::fs::remove_file(&fresh);
    })
}

/// Take the hooks folder's installer lock, waiting a while for another
/// installer, in this process or another, to finish.
fn hook_lock(hooks: &Path) -> Result<agentdocker_host::lock::Lock, String> {
    let path = hooks.join(HOOK_LOCK);
    // The lock opens its file by name; a link there would have it create
    // or open a file elsewhere.
    if std::fs::symlink_metadata(&path).is_ok_and(|meta| !meta.is_file()) {
        return Err(format!("{} is not a plain file", path.display()));
    }
    let deadline = std::time::Instant::now() + HOOK_LOCK_WAIT;
    loop {
        match agentdocker_host::lock::try_exclusive(&path) {
            Ok(Some(lock)) => return Ok(lock),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Ok(None) => {
                return Err("another installer held the hooks folder too long".to_owned());
            }
            Err(error) => return Err(format!("the hooks folder's lock: {error}")),
        }
    }
}

/// A hook's contents, read through any link: `None` when it is larger than
/// [`HOOK_BYTES`].
fn read_hook(hook: &Path) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    std::fs::File::open(hook)?
        .take(HOOK_BYTES + 1)
        .read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= HOOK_BYTES).then_some(bytes))
}

/// Put our post-commit hook in `hooks`, under the folder's installer lock
/// so that installers one after another each see what the last one left.
/// A hook there that is ours (it carries [`HOOK_MARK`]) is brought up to
/// date and never chained; any other is kept, moved to [`CHAINED`] by a
/// link that never replaces a file there, and run by ours after it
/// reports. A hook that cannot be read whole, or another hook beside one
/// already chained, is left as it is, and the reason said.
pub(super) fn install_hook(hooks: &Path, script: &str) -> Result<(), String> {
    use std::io::ErrorKind;
    let failed = |what: &str, error: std::io::Error| format!("{what}: {error}");
    std::fs::create_dir_all(hooks).map_err(|e| failed("the hooks folder", e))?;
    let _lock = hook_lock(hooks)?;
    let hook = hooks.join("post-commit");
    let chained = hooks.join(CHAINED);
    // Twice at most: a hook written meanwhile by something that takes no
    // lock is looked at again rather than written over.
    for _ in 0..2 {
        match std::fs::symlink_metadata(&hook) {
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {
                match write_executable(&hook, script) {
                    Ok(()) => return Ok(()),
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(failed("the hook", error)),
                }
            }
            Err(error) => return Err(failed("the post-commit hook", error)),
        }
        let Some(bytes) = read_hook(&hook).map_err(|e| failed("the post-commit hook", e))? else {
            return Err(format!(
                "{} is too large to tell whether it is AgentDocker's; nothing was changed",
                hook.display()
            ));
        };
        let mark = HOOK_MARK.as_bytes();
        if bytes.windows(mark.len()).any(|w| w == mark) {
            if bytes == script.as_bytes() {
                return Ok(());
            }
            // Ours, from another build: replaced whole.
            return replace_executable(hooks, &hook, script).map_err(|e| failed("the hook", e));
        }
        // Theirs: linked under the chained name, which fails rather than
        // replace a file already there, and only then unlinked here.
        match std::fs::hard_link(&hook, &chained) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                return Err(format!(
                    "{} is not AgentDocker's and {CHAINED} is there too; neither was changed",
                    hook.display()
                ));
            }
            Err(error) => return Err(failed("the existing hook", error)),
        }
        if let Err(error) = std::fs::remove_file(&hook) {
            let _ = std::fs::remove_file(&chained);
            return Err(failed("the existing hook", error));
        }
        match write_executable(&hook, script) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                // Theirs goes back where it was, so it still runs.
                if std::fs::hard_link(&chained, &hook).is_ok() {
                    let _ = std::fs::remove_file(&chained);
                }
                return Err(failed("the hook", error));
            }
        }
    }
    Err(format!(
        "{} kept changing while the hook was installed; it was left as it is",
        hook.display()
    ))
}

/// Write the witness file into a worktree's own git directory, readable by
/// this user alone, replacing one a removed worktree of the same name left.
fn write_witness(git_dir: &Path, witness: &WitnessFile) -> Result<(), String> {
    use std::io::Write as _;
    let path = git_dir.join(WITNESS_FILE);
    let text = serde_json::to_string(witness).map_err(|e| e.to_string())?;
    let file = match agentdocker_host::dirs::create_private_file(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::remove_file(&path).map_err(|e| e.to_string())?;
            agentdocker_host::dirs::create_private_file(&path)
        }
        other => other,
    };
    let mut file = file.map_err(|e| format!("the witness file: {e}"))?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("the witness file: {e}"))
}

/// Run git in `dir`: its standard output, trimmed, when it succeeds.
fn git_out(dir: &Path, args: &[&str]) -> Result<Option<String>, String> {
    let argv: Vec<String> = std::iter::once("git")
        .chain(args.iter().copied())
        .map(str::to_owned)
        .collect();
    let output = agentdocker_host::command::run(dir, &argv, std::time::Duration::from_secs(10))
        .map_err(|error| error.kind().to_string())?;
    Ok(output.success.then(|| output.stdout.trim().to_owned()))
}

/// The full ref a checkout's HEAD names, from git: `None` when HEAD is
/// detached, an error when git cannot say. Never `--short`: git lengthens
/// a short name it finds ambiguous (`heads/feature/mine` once a tag
/// `feature/mine` exists).
pub(super) fn head_ref(checkout: &Path) -> Result<Option<String>, String> {
    let argv: Vec<String> = ["git", "symbolic-ref", "--quiet", "HEAD"]
        .map(str::to_owned)
        .to_vec();
    let output =
        agentdocker_host::command::run(checkout, &argv, std::time::Duration::from_secs(10))
            .map_err(|error| error.kind().to_string())?;
    if output.success {
        return Ok(Some(output.stdout.trim().to_owned()));
    }
    // `--quiet` makes a detached HEAD a plain failure with nothing said.
    if output.text.trim().is_empty() {
        return Ok(None);
    }
    Err(output
        .text
        .trim()
        .lines()
        .next()
        .unwrap_or("git failed")
        .to_owned())
}

/// The commit a checkout's HEAD is at.
fn head_commit(checkout: &Path) -> Result<String, String> {
    git_out(
        checkout,
        &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
    )?
    .ok_or_else(|| "HEAD names no commit".to_owned())
}

/// The answer to a report that records nothing, and why.
fn not_recorded(note: &str) -> Response {
    Response::CommitWitnessed {
        recorded: false,
        agent: None,
        branch: None,
        note: Some(note.to_owned()),
    }
}

impl State {
    /// The witnessed worktree at `checkout` whose token `token` is: not
    /// found when the daemon witnesses no worktree there, forbidden when
    /// the token is another's.
    fn witnessed(
        &mut self,
        checkout: &Path,
        token: &SecretText,
    ) -> Result<WitnessedWorktree, Box<Response>> {
        let kept = self
            .store_read("witnessed worktree", |store| {
                store.witnessed_worktree(checkout)
            })
            .ok_or_else(|| {
                Box::new(self.storage_failure().unwrap_or_else(|| {
                    Response::error(
                        ErrorCode::StorageUnavailable,
                        "the witnessed worktree could not be read",
                    )
                }))
            })?;
        let Some(worktree) = kept else {
            return Err(Box::new(Response::error(
                ErrorCode::NotFound,
                format!(
                    "{} is not a worktree the daemon witnesses",
                    checkout.display()
                ),
            )));
        };
        if hash(token.expose()) != worktree.token_hash {
            return Err(Box::new(Response::error(
                ErrorCode::Forbidden,
                "that is not this worktree's witness token",
            )));
        }
        Ok(worktree)
    }

    /// Who may have made a commit in a witnessed worktree, in this order:
    /// the live agents holding an exclusive `path:` lease over the whole of
    /// it, then the live agent it was created for, by its current id. Never
    /// the person.
    fn worktree_owners(&self, worktree: &WitnessedWorktree) -> Vec<AgentId> {
        let key = ResourceKey::new(format!("path:{}", worktree.path.display()));
        let now = Utc::now();
        let live = |id: &AgentId| {
            let id = self.registry.canonical_id(id);
            self.registry
                .get(id)
                .filter(|r| r.status.is_live() && !humans::is_human(r))
                .map(|r| r.id.clone())
        };
        let mut owners: Vec<AgentId> = self
            .leases
            .all()
            .into_iter()
            .filter(|l| {
                l.mode == LeaseMode::Exclusive
                    && !l.is_expired(now)
                    && l.resource.overlaps(&key)
                    && l.resource.value().len() <= key.value().len()
            })
            .filter_map(|l| live(&l.holder))
            .collect();
        owners.extend(live(&worktree.agent));
        let mut seen = HashSet::new();
        owners.retain(|id| seen.insert(id.clone()));
        owners
    }

    /// Keep `authorship`, with its event when it is new: whether it was.
    pub(super) fn record_authorship(
        &mut self,
        authorship: &Authorship,
    ) -> Result<bool, Box<Response>> {
        let mut added = false;
        let committed = self.commit_produced(
            "authorship",
            |store, first| store.record_authorship(authorship, first),
            |_, _, was| added = was,
        );
        if committed != Persisted::Committed {
            return Err(Box::new(self.write_failure().unwrap_or_else(|| {
                Response::error(ErrorCode::Internal, "the commit's author was not recorded")
            })));
        }
        Ok(added)
    }

    /// Record that the daemon made `commit` for `agent` on `branch` (the
    /// full ref HEAD named), as soon as git has made it. The person, a
    /// detached HEAD, or an agent in no project records nothing. A refusal
    /// when it could not be kept.
    pub(super) fn record_daemon_commit(
        &mut self,
        agent: &AgentId,
        branch: Option<&str>,
        commit: &str,
    ) -> Option<Response> {
        let branch = branch?;
        let record = self.registry.get(agent).filter(|r| !humans::is_human(r))?;
        let authorship = Authorship {
            project: record.project.as_ref()?.id(),
            agent: agent.clone(),
            branch: branch.to_owned(),
            commit: commit.to_owned(),
            witness: Witness::Commit,
            at: Utc::now(),
        };
        authorship.check().ok()?;
        self.record_authorship(&authorship)
            .err()
            .map(|response| *response)
    }

    /// Whether `agent`, or a record folded into it, made a commit the
    /// daemon witnessed on `branch` in `project`, read by the branch's full
    /// ref.
    pub(super) fn has_authored(
        &mut self,
        project: &ProjectId,
        agent: &AgentId,
        branch: &str,
    ) -> Result<bool, Box<Response>> {
        let ids = self.registry.identity_ids(agent);
        let branch = gate::branch_ref(branch);
        self.store_read("authorship", |store| store.authored(project, &branch, &ids))
            .ok_or_else(|| {
                Box::new(self.storage_failure().unwrap_or_else(|| {
                    Response::error(
                        ErrorCode::StorageUnavailable,
                        "what the reviewer has committed could not be read",
                    )
                }))
            })
    }
}

impl Daemon {
    /// The `agentdocker` command a witness hook reports with.
    fn witness_cli(&self) -> Option<PathBuf> {
        lock(&self.witness_cli).clone()
    }

    /// Give the worktree at `path`, just created for `agent`, the hook that
    /// reports the commits made there, and keep it as witnessed. Why not,
    /// when it was not: commits made there with git alone are then not
    /// witnessed. Nothing here fails the worktree.
    pub(super) async fn witness_worktree(&self, path: &Path, agent: &AgentId) -> Option<String> {
        let Some(cli) = self.witness_cli() else {
            return Some(
                "there is no agentdocker command beside the daemon for a hook to report with"
                    .to_owned(),
            );
        };
        let Some(project) = self
            .project_for(Some(path.to_path_buf()), true)
            .await
            .map(|p| p.id())
        else {
            return Some("its project could not be told".to_owned());
        };
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let witness = WitnessFile {
            token: SecretText::new(token.clone()).expect("a token is short text"),
            socket: self.socket.clone(),
            checkout: path.to_path_buf(),
        };
        let script = hook_script(&cli);
        let dir = path.to_path_buf();
        let installed = tokio::task::spawn_blocking(move || {
            let git_dir = git_out(&dir, &["rev-parse", "--absolute-git-dir"])?
                .ok_or("git could not say where the worktree's git directory is")?;
            let common = git_out(&dir, &["rev-parse", "--git-common-dir"])?
                .ok_or("git could not say where the repository's hooks are")?;
            // A hooks path the person or a tool set (core.hooksPath) is
            // theirs: AgentDocker installs nothing there.
            if git_out(&dir, &["config", "--get", "core.hooksPath"])?.is_some_and(|p| !p.is_empty())
            {
                return Err("core.hooksPath is set, and AgentDocker installs no hook there".into());
            }
            install_hook(&dir.join(common).join("hooks"), &script)?;
            write_witness(Path::new(&git_dir), &witness)
        })
        .await
        .unwrap_or_else(|_| Err("the hook's installation did not complete".to_owned()));
        if let Err(why) = installed {
            warn!(path = %path.display(), %why, "commits in a new worktree are not witnessed");
            return Some(why);
        }
        let worktree = WitnessedWorktree {
            path: path.to_path_buf(),
            project,
            agent: agent.clone(),
            token_hash: hash(&token),
            created_at: Utc::now(),
        };
        let mut state = lock(&self.state);
        let event = Event::new(
            EventKind::WorktreeWitnessed {
                agent: agent.clone(),
                path: path.to_path_buf(),
            },
            worktree.created_at,
        );
        let _ = state.transition(
            "witnessed worktree",
            event,
            |store, event| store.witness_worktree(&worktree, event),
            |_| {},
        );
        state
            .write_failure()
            .map(|_| "the worktree's witness could not be kept".to_owned())
    }

    /// A commit a witnessed worktree's hook reported: the token must be the
    /// worktree's, HEAD and the ref it is on must be what git says now, and
    /// it is recorded as `agent`'s — whom the socket's authentication found
    /// the sender's session to be — or, without that, as the worktree's
    /// first owner. A detached HEAD, or a commit the daemon is making
    /// itself (its `commit` records that), records nothing.
    pub(super) async fn report_commit(
        &self,
        checkout: PathBuf,
        token: SecretText,
        head: String,
        branch: Option<String>,
        agent: Option<String>,
    ) -> Response {
        let checkout = project::canonical(&checkout);
        let head = head.trim().to_owned();
        let branch = branch
            .map(|b| b.trim().to_owned())
            .filter(|b| !b.is_empty());
        if !gate::full_commit(&head) {
            return Response::error(
                ErrorCode::Invalid,
                "a report names the full commit id HEAD is at",
            );
        }
        let (worktree, who) = {
            let mut state = lock(&self.state);
            if state.committing.contains(&checkout) {
                return not_recorded("the daemon is making this commit, and records it itself");
            }
            let worktree = match state.witnessed(&checkout, &token) {
                Ok(worktree) => worktree,
                Err(response) => return *response,
            };
            let owners = state.worktree_owners(&worktree);
            let who = match agent {
                Some(named) => {
                    let named = match state.resolve(&named) {
                        Ok(id) => id,
                        Err(response) => return *response,
                    };
                    if !owners.contains(&named) {
                        return Response::error(
                            ErrorCode::Forbidden,
                            "a commit there is recorded only for an agent the worktree was made for or that holds it",
                        );
                    }
                    named
                }
                None => match owners.into_iter().next() {
                    Some(owner) => owner,
                    None => {
                        return not_recorded(
                            "no live agent is the worktree's: neither the one it was made for nor a holder of it",
                        );
                    }
                },
            };
            (worktree, who)
        };
        // What git says now, off the lock. The hook reports from inside the
        // commit, so HEAD is still where the commit put it.
        let dir = checkout.clone();
        let read = tokio::task::spawn_blocking(move || {
            Ok::<_, String>((head_commit(&dir)?, head_ref(&dir)?))
        })
        .await
        .unwrap_or_else(|_| Err("the read did not complete".to_owned()));
        let (now_head, now_ref) = match read {
            Ok(read) => read,
            Err(reason) => {
                return Response::error(
                    ErrorCode::Unavailable,
                    format!("the worktree's HEAD could not be read: {reason}"),
                );
            }
        };
        if now_head != head || now_ref != branch {
            return Response::error(
                ErrorCode::Conflict,
                "the worktree's HEAD is not where the report says; nothing was recorded",
            );
        }
        let Some(branch) = now_ref.filter(|r| r.starts_with("refs/heads/")) else {
            return not_recorded("HEAD is detached, on no branch");
        };
        let authorship = Authorship {
            project: worktree.project.clone(),
            agent: who.clone(),
            branch: branch.clone(),
            commit: head,
            witness: Witness::Hook,
            at: Utc::now(),
        };
        if let Err(reason) = authorship.check() {
            return Response::error(ErrorCode::Invalid, reason);
        }
        let mut state = lock(&self.state);
        // What was found before git was read must still hold.
        if state.committing.contains(&checkout) {
            return not_recorded("the daemon is making this commit, and records it itself");
        }
        if !state.worktree_owners(&worktree).contains(&who) {
            return Response::error(
                ErrorCode::Conflict,
                "who holds the worktree changed while the report was checked; nothing was recorded",
            );
        }
        if let Some(error) = state.write_failure() {
            return error;
        }
        match state.record_authorship(&authorship) {
            Ok(added) => Response::CommitWitnessed {
                recorded: true,
                agent: Some(who),
                branch: Some(branch),
                note: (!added).then(|| "already recorded".to_owned()),
            },
            Err(response) => *response,
        }
    }

    /// Authenticate a hook's report as sent from inside the session of one
    /// of the worktree's owners, and name that owner in it, so that no
    /// other agent can make a commit somebody else's. A refusal, or a final
    /// answer, when it is not to be handled. Who made the commit is never
    /// the client's to say. With the person check off, and for the daemon's
    /// own callers, the handler names the worktree's first owner.
    pub(crate) async fn authenticate_report(
        self: &Arc<Self>,
        peer: Option<person::Peer>,
        request: &mut Request,
    ) -> Option<Response> {
        let Request::ReportCommit {
            checkout,
            token,
            agent,
            ..
        } = request
        else {
            return None;
        };
        *agent = None;
        if !self.person_check || peer.is_some_and(|(pid, _)| pid == std::process::id()) {
            return None;
        }
        let checkout = project::canonical(checkout);
        let (owners, agents) = {
            let mut state = lock(&self.state);
            if state.committing.contains(&checkout) {
                return Some(not_recorded(
                    "the daemon is making this commit, and records it itself",
                ));
            }
            let worktree = match state.witnessed(&checkout, token) {
                Ok(worktree) => worktree,
                Err(response) => return Some(*response),
            };
            let owners: Vec<AgentRecord> = state
                .worktree_owners(&worktree)
                .iter()
                .filter_map(|id| state.registry.get(id).cloned())
                .collect();
            if owners.is_empty() {
                return Some(not_recorded(
                    "no live agent is the worktree's: neither the one it was made for nor a holder of it",
                ));
            }
            let agents: HashMap<u32, (DateTime<Utc>, AgentId)> = state
                .registry
                .live()
                .filter(|r| !humans::is_human(r))
                .filter_map(|r| Some((r.pid?, (r.process_started_at?, r.id.clone()))))
                .collect();
            (owners, agents)
        };
        let refuse = |why: &str| {
            Some(Response::error(
                ErrorCode::Forbidden,
                format!(
                    "a commit is recorded only as reported from inside the session of the agent it names, and {why}"
                ),
            ))
        };
        let Some((sender, Some(sender_born))) = peer else {
            return refuse("the process that sent it cannot be identified");
        };
        if procinfo::start_time(sender) != Some(sender_born) {
            return refuse("the process that sent it has ended");
        }
        let Ok(Ok(table)) = tokio::task::spawn_blocking(procinfo::processes).await else {
            return refuse("its sender's parents could not be read");
        };
        let mut why = "it was sent from outside every owner's session";
        for owner in &owners {
            match tasks::verdicts::author(
                sender,
                owner,
                &agents,
                &table,
                procinfo::start_time,
                |pid| procinfo::executable_path_of(pid).ok(),
                |exe| {
                    person::companion_cli()
                        .is_some_and(|cli| agentdocker_host::program::same_file(&cli, exe))
                },
            ) {
                tasks::verdicts::Author::Reviewer => {
                    *agent = Some(owner.id.to_string());
                    return None;
                }
                tasks::verdicts::Author::Other(reason) => why = reason,
            }
        }
        warn!(peer = sender, %why, "refused a commit report");
        refuse(why)
    }
}

#[cfg(test)]
mod tests;
