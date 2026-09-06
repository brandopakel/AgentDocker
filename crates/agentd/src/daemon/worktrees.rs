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
        if evidence.environment != target_environment {
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
