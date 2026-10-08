//! What an agent changed on its branch, read against the branch in the
//! project's main checkout, and the person's one-step merge of it.
use super::worktrees::{Committing, git};
use super::*;
use agentdocker_core::review::{self, Review};

/// One git command's standard output when it succeeded, else nothing.
async fn read(root: &Path, args: &[&str]) -> Option<String> {
    match git(
        root.to_path_buf(),
        args.iter().map(|a| (*a).to_owned()).collect(),
    )
    .await
    {
        Ok(output) if output.success => Some(output.stdout),
        _ => None,
    }
}

fn line(text: Option<String>) -> Option<String> {
    text.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

/// Read the review for an agent's checkout `dir` in the repository whose
/// main checkout is `root`. `held` names what someone holds in `root`.
async fn gather(
    agent: AgentId,
    dir: PathBuf,
    root: PathBuf,
    held: Vec<(String, String)>,
) -> Review {
    let branch = line(read(&dir, &["symbolic-ref", "--short", "-q", "HEAD"]).await);
    let head = line(read(&dir, &["rev-parse", "HEAD"]).await);
    let target = line(read(&root, &["symbolic-ref", "--short", "-q", "HEAD"]).await);
    let mut review = Review {
        agent,
        checkout: dir.clone(),
        branch,
        head: head.clone(),
        target_checkout: root.clone(),
        target: target.clone(),
        held,
        ..Review::default()
    };
    if (dir == root && review.branch == review.target) || head.is_none() {
        return review;
    }
    let (Some(target), Some(head)) = (target, head) else {
        return review;
    };
    review.merge_base = line(read(&dir, &["merge-base", &target, &head]).await);
    if let Some(base) = review.merge_base.clone() {
        let range = format!("{base}..{head}");
        review.commits = review::parse_log(
            &read(
                &dir,
                &["log", "--max-count=200", "--format=%H%x1f%s", &range],
            )
            .await
            .unwrap_or_default(),
        );
        let name_status = read(&dir, &["diff", "--name-status", "-M", &base, &head]).await;
        let numstat = read(&dir, &["diff", "--numstat", "-M", &base, &head]).await;
        review.files = review::parse_files(
            &name_status.unwrap_or_default(),
            &numstat.unwrap_or_default(),
        );
        // A patch past the command's own 4 MiB bound reads as nothing; the
        // file list still says what changed.
        match read(
            &dir,
            &["diff", "--no-ext-diff", "--no-color", "-M", &base, &head],
        )
        .await
        {
            Some(patch) if patch.len() > review::PATCH_LIMIT => {
                let cut = patch[..review::PATCH_LIMIT]
                    .rfind('\n')
                    .unwrap_or(review::PATCH_LIMIT);
                review.patch = patch[..cut].to_owned();
                review.truncated = true;
            }
            Some(patch) => review.patch = patch,
            None => review.truncated = !review.files.is_empty(),
        }
    }
    review.uncommitted = review::parse_porcelain(
        &read(&dir, &["status", "--porcelain", "--untracked-files=no"])
            .await
            .unwrap_or_default(),
    );
    review.target_dirty = review::parse_porcelain(
        &read(&root, &["status", "--porcelain", "--untracked-files=no"])
            .await
            .unwrap_or_default(),
    );
    // A trial merge in git's object store: nothing in either checkout
    // moves. Exit 1 with a tree on its first line is a conflict; anything
    // else is not known, and is not reported as one.
    if !review.commits.is_empty()
        && let Some(target) = review.target.as_deref()
        && let Ok(output) = git(
            root.clone(),
            vec![
                "merge-tree".into(),
                "--write-tree".into(),
                "--name-only".into(),
                target.to_owned(),
                head.clone(),
            ],
        )
        .await
        && !output.success
        && output
            .stdout
            .lines()
            .next()
            .is_some_and(|tree| tree.len() >= 40 && tree.chars().all(|c| c.is_ascii_hexdigit()))
    {
        review.conflicts = review::parse_conflicts(&output.stdout);
    }
    review
}

impl Daemon {
    /// The checkout an agent worked in and its repository's main checkout,
    /// for a live or an ended agent while the checkout is still on disk.
    fn review_checkout(
        &self,
        reference: &str,
    ) -> Result<(AgentId, PathBuf, PathBuf), Box<Response>> {
        let mut state = lock(&self.state);
        let id = state.resolve(reference)?;
        let record = state.registry.get(&id).expect("resolved");
        let Some(project) = record.project.as_ref() else {
            return Err(Box::new(Response::error(
                ErrorCode::Invalid,
                "this agent works in no project, so there is no branch to review",
            )));
        };
        let dir = project::canonical(project.dir());
        let root = project::canonical(&project.root);
        if !dir.exists() {
            return Err(Box::new(Response::error(
                ErrorCode::NotFound,
                format!("its checkout {} is gone", dir.display()),
            )));
        }
        Ok((id, dir, root))
    }

    /// What someone holds in the main checkout: a merge would write there.
    fn held_in(&self, root: &Path) -> Vec<(String, String)> {
        let mut state = lock(&self.state);
        state.expire_leases();
        let key = ResourceKey::new(format!("path:{}", root.display()));
        state
            .leases
            .holders_of(&key)
            .into_iter()
            .filter(|lease| {
                state
                    .registry
                    .get(&lease.holder)
                    .is_none_or(|holder| !humans::is_human(holder))
            })
            .map(|lease| {
                let who = state
                    .registry
                    .get(&lease.holder)
                    .map(|r| r.spec.name.clone())
                    .unwrap_or_else(|| lease.holder.to_string());
                (lease.resource.value().to_owned(), who)
            })
            .collect()
    }

    pub(super) async fn review_branch(&self, reference: &str) -> Response {
        let (agent, dir, root) = match self.review_checkout(reference) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        let held = self.held_in(&root);
        Response::BranchReview {
            review: gather(agent, dir, root, held).await,
        }
    }

    /// Merge an agent's branch into the main checkout's branch with a
    /// merge commit — only if its head is still the one the person looked
    /// at and nothing stops it, and never leaving a half-made merge.
    pub(super) async fn merge_branch(&self, from: &str, reference: &str, head: &str) -> Response {
        let (agent, dir, root) = match self.review_checkout(reference) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        let held = self.held_in(&root);
        let review = gather(agent.clone(), dir, root.clone(), held).await;
        if review.head.as_deref() != Some(head) {
            return Response::error(
                ErrorCode::Conflict,
                "its branch moved since you looked; review it again",
            );
        }
        let target = review.target.clone().unwrap_or_default();
        let blockers = review.blockers();
        if !blockers.is_empty() {
            let said: Vec<String> = blockers.iter().map(|b| b.label(&target)).collect();
            return Response::error(
                ErrorCode::Conflict,
                format!("not merged: {}", said.join("; ")),
            );
        }
        let branch = review.branch.clone().unwrap_or_default();
        let person = {
            let mut state = lock(&self.state);
            state
                .resolve(from)
                .ok()
                .and_then(|id| state.registry.get(&id).cloned())
        };
        // Journaled here, against the person, rather than guessed by the
        // watcher; without a record for the person the watcher records it.
        let _committing = person
            .is_some()
            .then(|| Committing::mark(self, root.clone()));
        let before = line(read(&root, &["rev-parse", "HEAD"]).await);
        let message = format!("Merge branch '{branch}'");
        let merged = git(
            root.clone(),
            vec![
                "merge".into(),
                "--no-ff".into(),
                "-m".into(),
                message,
                head.to_owned(),
            ],
        )
        .await;
        let failed = match merged {
            Ok(output) if output.success => None,
            Ok(output) => Some(output.text),
            Err(e) => Some(e.to_string()),
        };
        if let Some(reason) = failed {
            // Nothing is left half-made: a merge git started is undone.
            let _ = git(root.clone(), vec!["merge".into(), "--abort".into()]).await;
            return Response::error(
                ErrorCode::Conflict,
                format!("not merged, and {target} is as it was: {}", reason.trim()),
            );
        }
        let Some(commit) = line(read(&root, &["rev-parse", "HEAD"]).await) else {
            return Response::error(
                ErrorCode::Internal,
                "merged, but the new head could not be read",
            );
        };
        let mut state = lock(&self.state);
        if let Some(person) = person {
            let short: String = commit.chars().take(7).collect();
            let summary = format!(
                "merged {branch} into {target} ({} commit{}) as {short}",
                review.commits.len(),
                if review.commits.len() == 1 { "" } else { "s" }
            );
            if let Some(mut entry) = state.plain_entry(
                &person,
                JournalKind::Commit,
                summary,
                SummarySource::Explicit,
            ) {
                entry.branch = Some(target.clone());
                entry.head_before = before;
                entry.head_after = Some(commit.clone());
                entry.paths_total = review.files.len();
                state.append_journal(entry);
            }
            state.last_head.insert(root.clone(), commit.clone());
        }
        state.emit(EventKind::BranchMerged {
            agent,
            branch,
            target: target.clone(),
            commit: commit.clone(),
        });
        Response::BranchMerged { target, commit }
    }
}

#[cfg(test)]
mod tests {
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

    async fn head_of(dir: &Path) -> String {
        line(read(dir, &["rev-parse", "HEAD"]).await).unwrap()
    }

    async fn reviewed(daemon: &Daemon) -> Review {
        match daemon.review_branch("otter").await {
            Response::BranchReview { review } => review,
            other => panic!("unexpected {other:?}"),
        }
    }

    fn refused(response: Response) -> bool {
        matches!(
            response,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        )
    }

    /// The person sees what an agent's branch brings and merges it with a
    /// merge commit in one step. A head they did not see, an uncommitted
    /// change in the main checkout, or a conflict is refused with nothing
    /// changed, and a merged branch has nothing left to merge.
    #[tokio::test]
    async fn the_person_reviews_an_agents_branch_and_merges_it_in_one_step() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        run(&root, &["init", "-q", "-b", "main"]).await;
        run(&root, &["config", "user.email", "test@example.com"]).await;
        run(&root, &["config", "user.name", "Test"]).await;
        std::fs::write(root.join("shared.txt"), "one\n").unwrap();
        run(&root, &["add", "."]).await;
        run(&root, &["commit", "-qm", "initial"]).await;
        let worktree = tmp.path().join("login");
        run(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat/login",
                worktree.to_str().unwrap(),
            ],
        )
        .await;
        let daemon =
            Arc::new(Daemon::open(tmp.path().join("state"), tmp.path().join("sock")).unwrap());
        daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "otter".into(),
                    workdir: Some(worktree.clone()),
                    ..AgentSpec::default()
                },
                pid: None,
                session: None,
            })
            .await;
        std::fs::write(worktree.join("login.rs"), "fn login() {}\n").unwrap();
        run(&worktree, &["add", "."]).await;
        run(&worktree, &["commit", "-qm", "Add login"]).await;

        let review = reviewed(&daemon).await;
        assert_eq!(review.branch.as_deref(), Some("feat/login"));
        assert_eq!(review.target.as_deref(), Some("main"));
        assert_eq!(review.commits.len(), 1);
        assert_eq!(review.commits[0].subject, "Add login");
        assert_eq!(review.files[0].path, "login.rs");
        assert!(review.patch.contains("+fn login() {}"), "{}", review.patch);
        assert!(review.blockers().is_empty(), "{:?}", review.blockers());
        let head = review.head.clone().unwrap();
        let before = head_of(&root).await;

        assert!(refused(
            daemon.merge_branch("user", "otter", "0000000").await
        ));
        std::fs::write(root.join("shared.txt"), "a local edit\n").unwrap();
        assert!(refused(daemon.merge_branch("user", "otter", &head).await));
        assert_eq!(head_of(&root).await, before, "nothing moved");
        run(&root, &["checkout", "--", "shared.txt"]).await;

        let Response::BranchMerged { target, commit } =
            daemon.merge_branch("user", "otter", &head).await
        else {
            panic!("the merge is made")
        };
        assert_eq!(target, "main");
        assert_eq!(head_of(&root).await, commit);
        assert!(root.join("login.rs").exists());
        let parents =
            line(read(&root, &["rev-list", "--parents", "-n", "1", "HEAD"]).await).unwrap();
        assert_eq!(
            parents.split_whitespace().count(),
            3,
            "a merge commit: {parents}"
        );
        assert_eq!(
            reviewed(&daemon).await.blockers(),
            [agentdocker_core::review::Blocker::NothingToMerge]
        );

        // Both sides changed one file: said before the merge, refused, and
        // main is left as it was, with no merge half-made.
        std::fs::write(worktree.join("shared.txt"), "the agent's\n").unwrap();
        run(&worktree, &["commit", "-qam", "Agent edit"]).await;
        std::fs::write(root.join("shared.txt"), "main's\n").unwrap();
        run(&root, &["commit", "-qam", "Main edit"]).await;
        let review = reviewed(&daemon).await;
        assert_eq!(review.conflicts, ["shared.txt"]);
        let before = head_of(&root).await;
        assert!(refused(
            daemon
                .merge_branch("user", "otter", review.head.as_deref().unwrap())
                .await
        ));
        assert_eq!(head_of(&root).await, before);
        assert!(!root.join(".git/MERGE_HEAD").exists());
    }
}
