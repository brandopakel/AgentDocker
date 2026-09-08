//! Explicit worktree creation and verified, uncommitted integration.
use super::*;
use agentdocker_core::Validation;
use agentdocker_host::{command, content};

async fn git(root: PathBuf, args: Vec<String>) -> anyhow::Result<command::Output> {
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
struct Committing<'a> {
    daemon: &'a Daemon,
    root: PathBuf,
}

impl<'a> Committing<'a> {
    fn mark(daemon: &'a Daemon, root: PathBuf) -> Self {
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
    match git(
        root,
        vec![
            "worktree".into(),
            "add".into(),
            "-b".into(),
            branch.to_owned(),
            "--".into(),
            path.to_string_lossy().into_owned(),
            "HEAD".into(),
        ],
    )
    .await
    {
        Ok(output) if output.success => Ok(()),
        Ok(output) => Err(Box::new(failure(output.text))),
        Err(e) => Err(Box::new(failure(e))),
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
    pub(super) async fn worktree_create(
        &self,
        reference: &str,
        path: String,
        branch: String,
    ) -> Response {
        let (agent, root, _) = match self.reader_checkout(reference) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        let path = match physical(path).await {
            Ok(p) => p,
            Err(e) => return failure(e),
        };
        if let Err(response) = add_worktree(root, &path, &branch).await {
            return *response;
        }
        lock(&self.state).emit(EventKind::WorktreeCreated {
            agent,
            path: path.clone(),
        });
        Response::Worktree { path, branch }
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
        if files == 0 {
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
        &self,
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
                    "feature".into()
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
