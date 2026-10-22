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

/// The git operation under way in the checkout at `root`, by the markers
/// in its own git directory (a linked worktree has its own).
async fn in_progress(root: &Path) -> Result<Option<review::Operation>, String> {
    let git_dir = PathBuf::from(
        line(read(root, &["rev-parse", "--absolute-git-dir"]).await)
            .ok_or("could not locate the main folder's git directory")?,
    );
    let mut present = Vec::new();
    for marker in [
        "rebase-merge",
        "rebase-apply",
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "BISECT_LOG",
        "sequencer",
    ] {
        match git_dir.join(marker).try_exists() {
            Ok(true) => present.push(marker),
            Ok(false) => {}
            Err(_) => return Err("could not read the main folder's git operation state".into()),
        }
    }
    Ok(review::Operation::from_markers(|marker| {
        present.contains(&marker)
    }))
}

/// A failed command cannot prove that a pending operation belongs to it.
/// In particular, a person may start a merge after our last marker read.
/// Leave Git's state available for inspection; never abort or reset here.
async fn merge_commit(root: &Path, head: &str, branch: &str) -> Result<(), String> {
    match git(
        root.to_path_buf(),
        vec![
            "merge".into(),
            "--no-ff".into(),
            "-m".into(),
            format!("Merge branch '{branch}'"),
            head.to_owned(),
        ],
    )
    .await
    {
        Ok(output) if output.success => Ok(()),
        Ok(output) => Err(output.text),
        Err(error) => Err(error.to_string()),
    }
}

/// Keep a large patch within its byte budget without splitting a UTF-8 character.
/// Prefer a complete line; a single oversized line still gets a safe prefix.
fn patch_prefix(patch: &str) -> &str {
    if patch.len() <= review::PATCH_LIMIT {
        return patch;
    }
    let mut boundary = review::PATCH_LIMIT;
    while !patch.is_char_boundary(boundary) {
        boundary -= 1;
    }
    let cut = patch[..boundary].rfind('\n').unwrap_or(boundary);
    &patch[..cut]
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
    // The display is best effort; the mutating path requires a successful
    // fresh operation read before it is allowed to run Git.
    review.in_progress = in_progress(&root).await.ok().flatten();
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
                review.patch = patch_prefix(&patch).to_owned();
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
    /// at and nothing stops it. On execution failure leave Git's state for
    /// the person to inspect; another operation may have started meanwhile.
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
            .then(|| Committing::mark(self, root.clone(), None));
        // A failed observation is not evidence that the checkout is free.
        match in_progress(&root).await {
            Ok(Some(operation)) => {
                return Response::error(
                    ErrorCode::Conflict,
                    format!(
                        "not merged: {}",
                        review::Blocker::InProgress(operation).label(&target)
                    ),
                );
            }
            Ok(None) => {}
            Err(reason) => return Response::error(ErrorCode::Unavailable, reason),
        }
        let before = line(read(&root, &["rev-parse", "HEAD"]).await);
        if let Err(reason) = merge_commit(&root, head, &branch).await {
            return Response::error(
                ErrorCode::Conflict,
                format!(
                    "Git did not confirm the merge; inspect {target} in the main folder before continuing: {}",
                    reason.trim()
                ),
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

    #[test]
    fn patch_budget_handles_every_utf8_boundary_and_complete_lines() {
        assert_eq!(patch_prefix("a short λ diff\n"), "a short λ diff\n");
        let exact = "x".repeat(review::PATCH_LIMIT);
        assert_eq!(patch_prefix(&exact), exact);
        for character in ['é', '界', '🦀'] {
            for split in 1..character.len_utf8() {
                let prefix = "x".repeat(review::PATCH_LIMIT - split);
                let patch = format!("{prefix}{character}tail");
                assert!(!patch.is_char_boundary(review::PATCH_LIMIT));
                assert_eq!(patch_prefix(&patch), prefix);

                let line = "complete header\n";
                let prefix = "x".repeat(review::PATCH_LIMIT - split - line.len());
                let patch = format!("{line}{prefix}{character}tail");
                assert!(!patch.is_char_boundary(review::PATCH_LIMIT));
                assert_eq!(patch_prefix(&patch), "complete header");
            }
        }
    }

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

        // The person's own merge under way in the main folder, with every
        // file already resolved: said, and never run into or undone.
        // Main takes the agent's line, so the branch would merge cleanly:
        // only the marker stands in the way. Before the check, git refused
        // the merge and the cleanup's `merge --abort` undid theirs.
        std::fs::write(root.join("shared.txt"), "the agent's\n").unwrap();
        run(&root, &["commit", "-qam", "Take the agent's line"]).await;
        let review = reviewed(&daemon).await;
        assert!(review.conflicts.is_empty(), "{:?}", review.conflicts);
        let head = review.head.unwrap();
        run(
            &root,
            &["merge", "--no-commit", "--no-ff", "-s", "ours", &head],
        )
        .await;
        assert!(
            read(&root, &["status", "--porcelain"])
                .await
                .unwrap()
                .is_empty()
        );
        let pending = operation_files(&root);
        let review = reviewed(&daemon).await;
        assert_eq!(review.in_progress, Some(review::Operation::Merge));
        assert_eq!(
            review.blockers(),
            [review::Blocker::InProgress(review::Operation::Merge)]
        );
        let before = head_of(&root).await;
        assert!(refused(daemon.merge_branch("user", "otter", &head).await));
        assert!(
            root.join(".git/MERGE_HEAD").exists(),
            "the person's merge is still theirs"
        );
        assert_eq!(head_of(&root).await, before);
        assert_eq!(operation_files(&root), pending);
    }

    /// Each marker git leaves names its operation, a rebase first, as git
    /// itself reads them.
    #[test]
    fn the_markers_in_a_git_directory_name_the_operation_under_way() {
        use review::Operation;
        let with = |markers: &'static [&'static str]| {
            Operation::from_markers(|marker| markers.contains(&marker))
        };
        assert_eq!(with(&[]), None);
        assert_eq!(with(&["MERGE_HEAD"]), Some(Operation::Merge));
        assert_eq!(
            with(&["rebase-merge", "MERGE_HEAD"]),
            Some(Operation::Rebase)
        );
        assert_eq!(with(&["rebase-apply"]), Some(Operation::Rebase));
        assert_eq!(with(&["CHERRY_PICK_HEAD"]), Some(Operation::CherryPick));
        assert_eq!(with(&["REVERT_HEAD"]), Some(Operation::Revert));
        assert_eq!(with(&["BISECT_LOG"]), Some(Operation::Bisect));
        assert_eq!(with(&["sequencer"]), Some(Operation::Other));
    }

    fn operation_files(root: &Path) -> Vec<Option<Vec<u8>>> {
        ["MERGE_HEAD", "MERGE_MSG", "MERGE_MODE", "ORIG_HEAD"]
            .iter()
            .map(|name| std::fs::read(root.join(".git").join(name)).ok())
            .collect()
    }

    async fn merge_fixture(root: &Path) -> String {
        run(root, &["init", "-q", "-b", "main"]).await;
        run(root, &["config", "user.email", "test@example.com"]).await;
        run(root, &["config", "user.name", "Test"]).await;
        run(root, &["config", "commit.gpgsign", "false"]).await;
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        run(root, &["add", "."]).await;
        run(root, &["commit", "-qm", "base"]).await;
        run(root, &["checkout", "-qb", "person"]).await;
        std::fs::write(root.join("person.txt"), "personal\n").unwrap();
        run(root, &["add", "."]).await;
        run(root, &["commit", "-qm", "personal"]).await;
        run(root, &["checkout", "-q", "main"]).await;
        run(root, &["checkout", "-qb", "agent"]).await;
        std::fs::write(root.join("agent.txt"), "agent\n").unwrap();
        run(root, &["add", "."]).await;
        run(root, &["commit", "-qm", "agent"]).await;
        let agent = head_of(root).await;
        run(root, &["checkout", "-q", "main"]).await;
        agent
    }

    #[tokio::test]
    async fn failed_merge_preserves_a_persons_operation_started_after_the_last_check() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let agent = merge_fixture(root).await;
        assert_eq!(in_progress(root).await.unwrap(), None);
        // The person acts after the final precheck, before our Git command.
        run(
            root,
            &["merge", "--no-commit", "--no-ff", "-s", "ours", "person"],
        )
        .await;
        assert!(
            read(root, &["status", "--porcelain"])
                .await
                .unwrap()
                .is_empty()
        );
        let before = operation_files(root);
        assert!(before[0].is_some());
        let head = head_of(root).await;
        assert!(merge_commit(root, &agent, "agent").await.is_err());
        assert_eq!(
            operation_files(root),
            before,
            "the person's merge remains intact"
        );
        assert_eq!(head_of(root).await, head);
    }

    #[tokio::test]
    async fn missing_git_directory_is_not_a_clean_operation_check() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(in_progress(&tmp.path().join("missing")).await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unreadable_marker_is_not_a_clean_operation_check() {
        let tmp = tempfile::tempdir().unwrap();
        merge_fixture(tmp.path()).await;
        let marker = tmp.path().join(".git/rebase-merge");
        std::os::unix::fs::symlink("rebase-merge", &marker).unwrap();
        assert!(in_progress(tmp.path()).await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejected_merge_commit_is_left_available_for_the_person_to_inspect() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let agent = merge_fixture(root).await;
        let hook = root.join(".git/hooks/pre-merge-commit");
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
        let head = head_of(root).await;
        assert!(merge_commit(root, &agent, "agent").await.is_err());
        assert_eq!(head_of(root).await, head);
        assert_eq!(
            std::fs::read_to_string(root.join(".git/MERGE_HEAD"))
                .unwrap()
                .trim(),
            agent
        );
        assert_eq!(
            std::fs::read_to_string(root.join("agent.txt")).unwrap(),
            "agent\n"
        );
        assert_eq!(
            in_progress(root).await.unwrap(),
            Some(review::Operation::Merge)
        );
    }
}
