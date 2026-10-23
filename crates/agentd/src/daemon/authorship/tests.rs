use super::*;
use agentdocker_core::gate::Findings;
use agentdocker_core::{AgentSpec, HUMAN, JournalEntry};
use tempfile::TempDir;

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// Run git in `dir`, which must succeed; its standard output, trimmed.
fn git_in(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Whether git here runs hooks from a path the person set for every
/// repository, where AgentDocker installs nothing: these tests then have
/// nothing to observe.
fn hooks_path_everywhere() -> bool {
    std::process::Command::new("git")
        .args(["config", "--get", "core.hooksPath"])
        .current_dir(std::env::temp_dir())
        .output()
        .is_ok_and(|output| output.status.success())
}

/// A repository on `branch` with one commit and an identity of its own,
/// so commits work without the machine's.
fn repository(path: &Path, branch: &str) {
    std::fs::create_dir_all(path).unwrap();
    git_in(path, &["init", "-q", "-b", branch]);
    git_in(path, &["config", "user.name", "t"]);
    git_in(path, &["config", "user.email", "t@example.com"]);
    git_in(path, &["config", "commit.gpgsign", "false"]);
    git_in(path, &["commit", "-q", "--allow-empty", "-m", "root"]);
}

fn open(dir: &TempDir) -> Arc<Daemon> {
    let home = dir.path().to_path_buf();
    Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap())
}

async fn register(daemon: &Arc<Daemon>, name: &str, workdir: &Path) -> AgentRecord {
    match daemon
        .handle(Request::Register {
            spec: AgentSpec {
                name: name.to_owned(),
                workdir: Some(workdir.to_owned()),
                ..AgentSpec::default()
            },
            pid: None,
            session: None,
        })
        .await
    {
        Response::Agent { agent } => agent,
        other => panic!("{other:?}"),
    }
}

fn give_role(daemon: &Arc<Daemon>, agent: &AgentId, roles: &str) {
    lock(&daemon.state)
        .registry
        .get_mut(agent)
        .unwrap()
        .spec
        .labels
        .insert("role".into(), roles.into());
}

fn verdict(agent: &str, project: Option<&str>, branch: &str, head: &str) -> Request {
    Request::CodeVerdict {
        agent: agent.into(),
        project: project.map(str::to_owned),
        branch: branch.into(),
        head: head.into(),
        from: None,
        verdict: "approve".into(),
        findings: Findings::default(),
        note: None,
    }
}

fn refusal(response: Response) -> Option<(ErrorCode, String)> {
    match response {
        Response::Error { code, message, .. } => Some((code, message)),
        _ => None,
    }
}

fn given(response: &Response) -> bool {
    matches!(response, Response::CodeVerdict { .. })
}

/// An `agentdocker` stand-in for the hook to run: it writes down how it
/// was run, one line each time.
fn logging_cli(dir: &Path) -> (PathBuf, PathBuf) {
    let cli = dir.join("fake-agentdocker");
    let log = dir.join("cli.log");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\n",
            sh_quote(&log.to_string_lossy())
        ),
    )
    .unwrap();
    make_executable(&cli);
    (cli, log)
}

fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn runs(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

async fn create_worktree(
    daemon: &Arc<Daemon>,
    agent: &AgentId,
    path: &Path,
    branch: &str,
) -> (PathBuf, Option<String>) {
    match daemon
        .handle(Request::WorktreeCreate {
            agent: agent.to_string(),
            path: path.to_string_lossy().into_owned(),
            branch: branch.into(),
            from: None,
            seed_build: Vec::new(),
        })
        .await
    {
        Response::Worktree {
            path, unwitnessed, ..
        } => (path, unwitnessed),
        other => panic!("{other:?}"),
    }
}

fn witness_of(worktree: &Path) -> WitnessFile {
    let git_dir = PathBuf::from(git_in(worktree, &["rev-parse", "--absolute-git-dir"]));
    serde_json::from_str(&std::fs::read_to_string(git_dir.join(WITNESS_FILE)).unwrap()).unwrap()
}

/// What `agentdocker report-commit` sends from `worktree`: its witness
/// token, HEAD and the full ref HEAD names.
fn report_of(worktree: &Path) -> Request {
    let witness = witness_of(worktree);
    let branch = std::process::Command::new("git")
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .current_dir(worktree)
        .output()
        .unwrap();
    Request::ReportCommit {
        checkout: witness.checkout,
        token: witness.token,
        head: git_in(worktree, &["rev-parse", "HEAD"]),
        branch: branch
            .status
            .success()
            .then(|| String::from_utf8_lossy(&branch.stdout).trim().to_owned()),
        agent: None,
    }
}

fn recorded_events(daemon: &Arc<Daemon>) -> Vec<(AgentId, String, String, Witness)> {
    daemon
        .recent_events(500)
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::AuthorshipRecorded {
                agent,
                branch,
                commit,
                witness,
                ..
            } => Some((agent, branch, commit, witness)),
            _ => None,
        })
        .collect()
}

fn kept(daemon: &Arc<Daemon>, agent: &AgentId) -> Vec<Authorship> {
    lock(&daemon.state).store.authorship_of(agent).unwrap()
}

async fn commit_entries(daemon: &Arc<Daemon>, project: &ProjectId) -> Vec<JournalEntry> {
    let journal: Request = serde_json::from_value(serde_json::json!({
        "op": "journal", "project": project.as_str(), "kind": "commit", "limit": 500
    }))
    .unwrap();
    match daemon.handle(journal).await {
        Response::Journal { entries, .. } => entries,
        other => panic!("{other:?}"),
    }
}

/// The card's case through the daemon's own commit: the journal loses
/// every line of it to a prune and to retention, the checkout moves to
/// another branch, and the word of the agent that committed is still
/// refused on the branch it wrote. A tag named like the branch hides
/// nothing. Its word on other branches, and the person's on this one,
/// count as ever.
#[tokio::test]
async fn a_branch_committed_to_stays_refused_after_the_journal_and_the_checkout_move_on() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = open(&tmp);
    let repo = tmp.path().join("repo");
    repository(&repo, "feature/mine");
    git_in(&repo, &["tag", "feature/mine"]);
    let reviewer = register(&daemon, "reviewer", &repo).await;
    give_role(&daemon, &reviewer.id, "reviewer");
    let project = reviewer.project.as_ref().unwrap().id();

    let mut made = Vec::new();
    for _ in 0..2 {
        std::fs::write(repo.join("work.txt"), format!("{}\n", made.len())).unwrap();
        git_in(&repo, &["add", "."]);
        match daemon
            .handle(Request::Commit {
                agent: reviewer.id.to_string(),
                message: "mine".into(),
                all: false,
                push: false,
            })
            .await
        {
            Response::Committed { head, branch, .. } => {
                assert_eq!(branch.as_deref(), Some("feature/mine"), "never heads/…");
                made.push(head);
            }
            other => panic!("{other:?}"),
        }
    }
    let ours: Vec<_> = recorded_events(&daemon)
        .into_iter()
        .map(|(agent, branch, commit, witness)| {
            assert_eq!(agent, reviewer.id);
            assert_eq!(branch, "refs/heads/feature/mine");
            assert_eq!(witness, Witness::Commit);
            commit
        })
        .collect();
    assert_eq!(ours, made);

    // The journal forgets: the first commit by request, the rest by
    // retention once it is older than the window.
    let commits = commit_entries(&daemon, &project).await;
    assert_eq!(commits.len(), 2, "{commits:?}");
    let Response::Pruned { removed } = daemon
        .handle(Request::JournalPrune {
            project: project.to_string(),
            before_seq: Some(commits[1].seq),
            older_than_secs: None,
        })
        .await
    else {
        panic!("no prune");
    };
    assert!(removed >= 1);
    {
        let state = lock(&daemon.state);
        let last = state.store.max_journal_seq(&project).unwrap();
        for seq in commits[1].seq..=last {
            state
                .store
                .age_journal_for_test(&project, seq, Utc::now() - Duration::days(40));
        }
    }
    std::fs::write(
        daemon.home.join("agentd.toml"),
        "[journal]\nretention = \"20d\"\n",
    )
    .unwrap();
    daemon.apply_journal_retention();
    assert!(commit_entries(&daemon, &project).await.is_empty());

    // The checkout moves on; the branch it wrote is still its own.
    git_in(&repo, &["checkout", "-q", "-b", "elsewhere"]);
    let (code, message) = refusal(
        daemon
            .handle(verdict(reviewer.id.as_str(), None, "feature/mine", A))
            .await,
    )
    .expect("refused");
    assert_eq!(code, ErrorCode::Forbidden);
    assert!(message.contains("committed to"), "{message}");
    assert!(
        given(
            &daemon
                .handle(verdict(reviewer.id.as_str(), None, "feature/x", A))
                .await
        ),
        "another branch is its to review"
    );
    assert!(
        given(
            &daemon
                .handle(verdict(HUMAN, Some(project.as_str()), "feature/mine", A))
                .await
        ),
        "the person's word counts"
    );
}

/// A worktree the daemon creates reports the commits made there with git;
/// going to a branch to read it — a checkout, a fast-forward — reports
/// nothing and leaves the reviewer free to review it, while a commit made
/// there is the reviewer's for good, the checkout gone elsewhere.
#[tokio::test]
async fn a_witnessed_commit_is_authorship_and_checking_a_branch_out_is_not() {
    if hooks_path_everywhere() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let daemon = open(&tmp);
    let (cli, log) = logging_cli(tmp.path());
    *lock(&daemon.witness_cli) = Some(cli.clone());
    let repo = tmp.path().join("repo");
    repository(&repo, "main");
    // Somebody else's branch, and a commit of theirs on top of it, made
    // without touching any checkout.
    let tree = git_in(&repo, &["rev-parse", "HEAD^{tree}"]);
    let root = git_in(&repo, &["rev-parse", "HEAD"]);
    let x1 = git_in(&repo, &["commit-tree", &tree, "-p", &root, "-m", "x1"]);
    git_in(&repo, &["branch", "feature/x", &x1]);
    let x2 = git_in(&repo, &["commit-tree", &tree, "-p", &x1, "-m", "x2"]);
    git_in(&repo, &["branch", "upstream", &x2]);
    let reviewer = register(&daemon, "reviewer", &repo).await;
    give_role(&daemon, &reviewer.id, "reviewer");

    let (wt, unwitnessed) =
        create_worktree(&daemon, &reviewer.id, &tmp.path().join("wt"), "review").await;
    assert_eq!(unwitnessed, None);
    let hook = std::fs::read_to_string(repo.join(".git/hooks/post-commit")).unwrap();
    assert!(hook.contains(HOOK_MARK) && hook.contains(&*cli.to_string_lossy()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let git_dir = git_in(&wt, &["rev-parse", "--absolute-git-dir"]);
        let mode = std::fs::metadata(Path::new(&git_dir).join(WITNESS_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the witness file is the user's alone");
    }
    assert!(daemon.recent_events(50).iter().any(|e| matches!(&e.kind,
        EventKind::WorktreeWitnessed { agent, path } if *agent == reviewer.id && *path == wt)));

    // Reading somebody's branch: onto it, brought up to date, and back.
    git_in(&wt, &["checkout", "-q", "feature/x"]);
    git_in(&wt, &["merge", "-q", "--ff-only", "upstream"]);
    git_in(&wt, &["checkout", "-q", "review"]);
    assert!(runs(&log).is_empty(), "no commit, no report");
    assert!(recorded_events(&daemon).is_empty());
    assert!(
        given(
            &daemon
                .handle(verdict(reviewer.id.as_str(), None, "feature/x", &x2))
                .await
        ),
        "checking a branch out is not writing it"
    );

    // A commit made there with git is reported, and is the reviewer's.
    git_in(&wt, &["checkout", "-q", "-b", "feature/y"]);
    git_in(&wt, &["commit", "-q", "--allow-empty", "-m", "mine"]);
    let mine = git_in(&wt, &["rev-parse", "HEAD"]);
    let witness_path = Path::new(&git_in(&wt, &["rev-parse", "--absolute-git-dir"]))
        .join(WITNESS_FILE)
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        runs(&log),
        [format!("report-commit --witness {witness_path}")]
    );
    match daemon.handle(report_of(&wt)).await {
        Response::CommitWitnessed {
            recorded: true,
            agent,
            branch,
            note: None,
        } => {
            assert_eq!(agent.as_ref(), Some(&reviewer.id));
            assert_eq!(branch.as_deref(), Some("refs/heads/feature/y"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        recorded_events(&daemon),
        [(
            reviewer.id.clone(),
            "refs/heads/feature/y".to_owned(),
            mine.clone(),
            Witness::Hook
        )]
    );
    git_in(&wt, &["checkout", "-q", "review"]);
    let (code, message) = refusal(
        daemon
            .handle(verdict(reviewer.id.as_str(), None, "feature/y", &mine))
            .await,
    )
    .expect("refused");
    assert_eq!(code, ErrorCode::Forbidden, "{message}");
    assert!(message.contains("committed to"), "{message}");
}

/// A commit on a detached HEAD names no branch and records nothing; a
/// report must carry the worktree's own token, name HEAD as it is, and
/// name only the worktree's owners; the same commit reported twice is
/// recorded once.
#[tokio::test]
async fn reports_are_checked_and_a_detached_commit_records_nothing() {
    if hooks_path_everywhere() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let daemon = open(&tmp);
    let (cli, log) = logging_cli(tmp.path());
    *lock(&daemon.witness_cli) = Some(cli);
    let repo = tmp.path().join("repo");
    repository(&repo, "main");
    let owner = register(&daemon, "owner", &repo).await;
    let other = register(&daemon, "other", &repo).await;
    let (wt, _) = create_worktree(&daemon, &owner.id, &tmp.path().join("wt"), "owned").await;

    git_in(&wt, &["checkout", "-q", "--detach"]);
    git_in(&wt, &["commit", "-q", "--allow-empty", "-m", "detached"]);
    assert_eq!(runs(&log).len(), 1, "the hook ran");
    match daemon.handle(report_of(&wt)).await {
        Response::CommitWitnessed {
            recorded: false,
            note: Some(note),
            ..
        } => assert!(note.contains("detached"), "{note}"),
        other => panic!("{other:?}"),
    }
    assert!(kept(&daemon, &owner.id).is_empty());
    assert!(recorded_events(&daemon).is_empty());

    git_in(&wt, &["checkout", "-q", "owned"]);
    git_in(&wt, &["commit", "-q", "--allow-empty", "-m", "on a branch"]);
    let honest = report_of(&wt);
    // Another worktree's token, or none of this one's: forbidden.
    let mut forged = honest.clone();
    if let Request::ReportCommit { token, .. } = &mut forged {
        *token = SecretText::new("0".repeat(64)).unwrap();
    }
    assert_eq!(
        refusal(daemon.handle(forged).await).map(|(code, _)| code),
        Some(ErrorCode::Forbidden)
    );
    // Named for an agent the worktree is not: forbidden.
    let mut misnamed = honest.clone();
    if let Request::ReportCommit { agent, .. } = &mut misnamed {
        *agent = Some(other.id.to_string());
    }
    assert_eq!(
        refusal(daemon.handle(misnamed).await).map(|(code, _)| code),
        Some(ErrorCode::Forbidden)
    );
    // A HEAD that is not the worktree's: nothing recorded.
    let mut elsewhere = honest.clone();
    if let Request::ReportCommit { head, .. } = &mut elsewhere {
        *head = A.to_owned();
    }
    assert_eq!(
        refusal(daemon.handle(elsewhere).await).map(|(code, _)| code),
        Some(ErrorCode::Conflict)
    );
    let mut renamed = honest.clone();
    if let Request::ReportCommit { branch, .. } = &mut renamed {
        *branch = Some("refs/heads/main".into());
    }
    assert_eq!(
        refusal(daemon.handle(renamed).await).map(|(code, _)| code),
        Some(ErrorCode::Conflict)
    );
    // A worktree the daemon did not make: not found.
    let mut stranger = honest.clone();
    if let Request::ReportCommit { checkout, .. } = &mut stranger {
        *checkout = repo.clone();
    }
    assert_eq!(
        refusal(daemon.handle(stranger).await).map(|(code, _)| code),
        Some(ErrorCode::NotFound)
    );
    assert!(kept(&daemon, &owner.id).is_empty() && kept(&daemon, &other.id).is_empty());

    // Once, however often it is said.
    for again in [false, true] {
        match daemon.handle(honest.clone()).await {
            Response::CommitWitnessed {
                recorded: true,
                agent,
                note,
                ..
            } => {
                assert_eq!(agent.as_ref(), Some(&owner.id));
                assert_eq!(note.is_some(), again, "{note:?}");
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(recorded_events(&daemon).len(), 1);
    assert_eq!(kept(&daemon, &owner.id).len(), 1);
}

/// An agent that commits and then ends is still the author of what it
/// committed: the record was made at the commit, and outlives the agent's
/// exit and the daemon's restart, so the same record taken up again is
/// refused on that branch.
#[tokio::test]
async fn an_agent_that_commits_and_exits_still_counts() {
    if hooks_path_everywhere() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let daemon = open(&tmp);
    let (cli, _) = logging_cli(tmp.path());
    *lock(&daemon.witness_cli) = Some(cli);
    let repo = tmp.path().join("repo");
    repository(&repo, "main");
    let agent = register(&daemon, "writer", &repo).await;
    give_role(&daemon, &agent.id, "reviewer");
    let project = agent.project.as_ref().unwrap().id();
    let (wt, _) = create_worktree(&daemon, &agent.id, &tmp.path().join("wt"), "feature/w").await;
    git_in(&wt, &["commit", "-q", "--allow-empty", "-m", "mine"]);
    let mine = git_in(&wt, &["rev-parse", "HEAD"]);
    assert!(matches!(
        daemon.handle(report_of(&wt)).await,
        Response::CommitWitnessed { recorded: true, .. }
    ));
    assert!(matches!(
        daemon
            .handle(Request::Deregister {
                agent: agent.id.to_string()
            })
            .await,
        Response::Agent { .. }
    ));
    assert!(
        lock(&daemon.state)
            .has_authored(&project, &agent.id, "feature/w")
            .unwrap()
    );
    drop(daemon);

    let daemon = open(&tmp);
    assert!(
        lock(&daemon.state)
            .has_authored(&project, &agent.id, "feature/w")
            .unwrap(),
        "kept across a restart"
    );
    // The same record, running again (as a reconnected session is), with
    // its role.
    lock(&daemon.state)
        .registry
        .get_mut(&agent.id)
        .unwrap()
        .status = AgentStatus::Running;
    give_role(&daemon, &agent.id, "reviewer");
    let (code, message) = refusal(
        daemon
            .handle(verdict(agent.id.as_str(), None, "feature/w", &mine))
            .await,
    )
    .expect("refused");
    assert_eq!(code, ErrorCode::Forbidden, "{message}");
    assert!(message.contains("committed to"), "{message}");
    assert!(
        given(
            &daemon
                .handle(verdict(agent.id.as_str(), None, "feature/v", &mine))
                .await
        ),
        "it reviews what it did not write"
    );
}

/// A post-commit hook the repository already had keeps running, in the
/// worktree and in every other checkout, after AgentDocker's is put in
/// front of it; a second worktree leaves both as they are; a hooks path
/// set for the repository gets nothing.
#[tokio::test]
async fn a_hook_already_there_still_runs() {
    if hooks_path_everywhere() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let daemon = open(&tmp);
    let (cli, log) = logging_cli(tmp.path());
    *lock(&daemon.witness_cli) = Some(cli);
    let repo = tmp.path().join("repo");
    repository(&repo, "main");
    let marker = tmp.path().join("theirs.log");
    let theirs = format!(
        "#!/bin/sh\necho \"$(git rev-parse --show-toplevel)\" >> {}\n",
        sh_quote(&marker.to_string_lossy())
    );
    let hooks = repo.join(".git/hooks");
    std::fs::write(hooks.join("post-commit"), &theirs).unwrap();
    make_executable(&hooks.join("post-commit"));
    let agent = register(&daemon, "writer", &repo).await;

    let (wt, unwitnessed) =
        create_worktree(&daemon, &agent.id, &tmp.path().join("wt"), "feature/w").await;
    assert_eq!(unwitnessed, None);
    assert_eq!(
        std::fs::read_to_string(hooks.join(CHAINED)).unwrap(),
        theirs,
        "kept whole, under another name"
    );
    let ours = std::fs::read_to_string(hooks.join("post-commit")).unwrap();
    assert!(ours.contains(HOOK_MARK));

    git_in(
        &wt,
        &["commit", "-q", "--allow-empty", "-m", "in the worktree"],
    );
    git_in(
        &repo,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "in the main checkout",
        ],
    );
    let ran: Vec<PathBuf> = std::fs::read_to_string(&marker)
        .unwrap()
        .lines()
        .map(|l| project::canonical(Path::new(l)))
        .collect();
    assert_eq!(
        ran,
        [project::canonical(&wt), project::canonical(&repo)],
        "theirs ran both times"
    );
    assert_eq!(runs(&log).len(), 1, "only the witnessed worktree reports");

    // A second worktree: ours stays, theirs stays chained once.
    let (_, unwitnessed) =
        create_worktree(&daemon, &agent.id, &tmp.path().join("wt2"), "feature/v").await;
    assert_eq!(unwitnessed, None);
    assert_eq!(
        std::fs::read_to_string(hooks.join("post-commit")).unwrap(),
        ours
    );
    assert_eq!(
        std::fs::read_to_string(hooks.join(CHAINED)).unwrap(),
        theirs
    );

    // A repository whose hooks live where the person said: nothing goes there.
    let managed = tmp.path().join("managed");
    repository(&managed, "main");
    git_in(&managed, &["config", "core.hooksPath", ".githooks"]);
    let elsewhere = register(&daemon, "elsewhere", &managed).await;
    let (_, unwitnessed) =
        create_worktree(&daemon, &elsewhere.id, &tmp.path().join("wt3"), "feature/u").await;
    assert!(
        unwitnessed
            .as_deref()
            .is_some_and(|why| why.contains("core.hooksPath")),
        "{unwitnessed:?}"
    );
    assert!(!managed.join(".git/hooks/post-commit").exists());
    assert!(!managed.join(".githooks").exists());
}

/// Ours from another build is brought up to date in place; a hook that
/// is not ours beside one already chained is left alone, and says so.
#[test]
fn installing_never_loses_a_hook() {
    let tmp = tempfile::tempdir().unwrap();
    let hooks = tmp.path().join("hooks");
    let script = hook_script(Path::new("/opt/it's/agentdocker"));
    assert!(
        script.contains(r"cli='/opt/it'\''s/agentdocker'"),
        "{script}"
    );
    install_hook(&hooks, &script).unwrap();
    assert_eq!(
        std::fs::read_to_string(hooks.join("post-commit")).unwrap(),
        script
    );
    assert!(!hooks.join(CHAINED).exists(), "nothing to chain");
    let newer = hook_script(Path::new("/usr/local/bin/agentdocker"));
    install_hook(&hooks, &newer).unwrap();
    assert_eq!(
        std::fs::read_to_string(hooks.join("post-commit")).unwrap(),
        newer
    );
    assert!(
        !hooks.join(CHAINED).exists(),
        "ours is replaced, not chained"
    );

    std::fs::write(hooks.join("post-commit"), "#!/bin/sh\necho mine\n").unwrap();
    install_hook(&hooks, &newer).unwrap();
    assert_eq!(
        std::fs::read_to_string(hooks.join(CHAINED)).unwrap(),
        "#!/bin/sh\necho mine\n"
    );
    std::fs::write(hooks.join("post-commit"), "#!/bin/sh\necho another\n").unwrap();
    let why = install_hook(&hooks, &newer).unwrap_err();
    assert!(why.contains(CHAINED), "{why}");
    assert_eq!(
        std::fs::read_to_string(hooks.join("post-commit")).unwrap(),
        "#!/bin/sh\necho another\n"
    );
    assert_eq!(
        std::fs::read_to_string(hooks.join(CHAINED)).unwrap(),
        "#!/bin/sh\necho mine\n"
    );
}

/// The daemon's own commit in a witnessed worktree runs the hook too; the
/// report it sends while the commit is in flight records nothing, and the
/// commit path records the commit once, as the agent's that asked.
#[tokio::test]
async fn the_daemon_s_own_commit_in_a_witnessed_worktree_is_recorded_once() {
    if hooks_path_everywhere() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let daemon = open(&tmp);
    let (cli, log) = logging_cli(tmp.path());
    *lock(&daemon.witness_cli) = Some(cli);
    let repo = tmp.path().join("repo");
    repository(&repo, "main");
    let maker = register(&daemon, "maker", &repo).await;
    let (wt, _) = create_worktree(&daemon, &maker.id, &tmp.path().join("wt"), "feature/w").await;
    let writer = register(&daemon, "writer", &wt).await;
    std::fs::write(wt.join("work.txt"), "x\n").unwrap();
    git_in(&wt, &["add", "."]);
    // As if the hook's report arrived while git was still committing.
    let in_flight = crate::daemon::worktrees::Committing::mark(&daemon, project::canonical(&wt));
    assert!(matches!(
        daemon.handle(report_of(&wt)).await,
        Response::CommitWitnessed {
            recorded: false,
            ..
        }
    ));
    drop(in_flight);
    let Response::Committed { head, .. } = daemon
        .handle(Request::Commit {
            agent: writer.id.to_string(),
            message: "through the daemon".into(),
            all: false,
            push: false,
        })
        .await
    else {
        panic!("no commit");
    };
    assert_eq!(runs(&log).len(), 1, "git ran the hook");
    assert_eq!(
        recorded_events(&daemon),
        [(
            writer.id.clone(),
            "refs/heads/feature/w".to_owned(),
            head,
            Witness::Commit
        )]
    );
}

/// Over the real socket, with the hook git runs: a commit made from
/// inside the session of the agent the worktree was made for is recorded
/// as that agent's, the hook that was already there runs as well, and the
/// same report sent from inside another agent's session, or from no
/// owner's, is refused.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_owner_s_session_reports_its_commit() {
    if hooks_path_everywhere() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let daemon = Arc::new(Daemon::open(tmp.path().into(), tmp.path().join("s")).unwrap());
    let listener = crate::server::bind(&daemon).await.unwrap();
    let serving = tokio::spawn(crate::server::serve(daemon.clone(), listener));
    let replies = tmp.path().join("replies");
    // An `agentdocker report-commit` that sends what the real one does.
    let cli = tmp.path().join("sending-agentdocker");
    std::fs::write(
        &cli,
        format!(
            r#"#!/bin/sh
exec python3 - "$3" {} <<'PY'
import json, socket, subprocess, sys
w = json.load(open(sys.argv[1]))
d = w["checkout"]
head = subprocess.run(["git", "-C", d, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
ref = subprocess.run(["git", "-C", d, "symbolic-ref", "--quiet", "HEAD"], capture_output=True, text=True)
request = {{"op": "report_commit", "checkout": d, "token": w["token"], "head": head,
           "branch": ref.stdout.strip() if ref.returncode == 0 else None}}
s = socket.socket(socket.AF_UNIX)
s.connect(w["socket"])
s.sendall((json.dumps(request) + "\n").encode())
reply = s.makefile().readline()
with open(sys.argv[2], "a") as f:
    f.write(reply)
PY
"#,
            sh_quote(&replies.to_string_lossy())
        ),
    )
    .unwrap();
    make_executable(&cli);
    *lock(&daemon.witness_cli) = Some(cli.clone());
    let repo = tmp.path().join("repo");
    repository(&repo, "main");
    let marker = tmp.path().join("theirs.log");
    std::fs::write(
        repo.join(".git/hooks/post-commit"),
        format!(
            "#!/bin/sh\necho ran >> {}\n",
            sh_quote(&marker.to_string_lossy())
        ),
    )
    .unwrap();
    make_executable(&repo.join(".git/hooks/post-commit"));

    // A session that commits in the worktree when told to.
    let wt = tmp.path().join("wt");
    let (go, done) = (tmp.path().join("go"), tmp.path().join("done"));
    let session = |script: &str| {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .env("WT", &wt)
            .env("GO", &go)
            .env("DONE", &done)
            .env("CLI", &cli)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut owner_process = session(
        r#"while [ ! -f "$GO" ]; do sleep 0.05; done
git -C "$WT" -c user.name=t -c user.email=t@example.com -c commit.gpgsign=false commit -q --allow-empty -m mine
touch "$DONE"; sleep 60"#,
    );
    let register_as = |name: &'static str, pid: u32| {
        let daemon = daemon.clone();
        let repo = repo.clone();
        async move {
            match daemon
                .handle(Request::Register {
                    spec: AgentSpec {
                        name: name.into(),
                        workdir: Some(repo),
                        ..AgentSpec::default()
                    },
                    pid: Some(pid),
                    session: None,
                })
                .await
            {
                Response::Agent { agent } => agent,
                other => panic!("{other:?}"),
            }
        }
    };
    let owner = register_as("owner", owner_process.id()).await;
    let (wt, unwitnessed) = create_worktree(&daemon, &owner.id, &wt, "feature/o").await;
    assert_eq!(unwitnessed, None);
    std::fs::write(&go, "").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !done.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the commit never ended"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let mine = git_in(&wt, &["rev-parse", "HEAD"]);
    let said: Vec<Response> = std::fs::read_to_string(&replies)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(
        matches!(&said[..], [Response::CommitWitnessed { recorded: true, agent: Some(a), .. }] if *a == owner.id),
        "{said:?}"
    );
    assert_eq!(
        kept(&daemon, &owner.id)
            .iter()
            .map(|a| (a.branch.as_str(), a.commit.as_str(), a.witness))
            .collect::<Vec<_>>(),
        [("refs/heads/feature/o", mine.as_str(), Witness::Hook)]
    );
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        "ran\n",
        "the hook that was there ran too"
    );

    // Another agent sending the owner's report from its own session, with
    // the owner's own token: refused.
    let mut forger_process = session(
        r#"while [ ! -f "$GO.2" ]; do sleep 0.05; done
"$CLI" report-commit --witness "$(git -C "$WT" rev-parse --absolute-git-dir)/agentdocker-witness"
touch "$DONE.2"; sleep 60"#,
    );
    let forger = register_as("forger", forger_process.id()).await;
    std::fs::write(go.with_extension("2"), "").unwrap();
    let finished = done.with_extension("2");
    while !finished.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the forger never ended"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    // And a process in no owner's session at all.
    let outsider = std::process::Command::new(&cli)
        .args(["report-commit", "--witness"])
        .arg(Path::new(&git_in(&wt, &["rev-parse", "--absolute-git-dir"])).join(WITNESS_FILE))
        .status()
        .unwrap();
    assert!(outsider.success());
    let said: Vec<Response> = std::fs::read_to_string(&replies)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(said.len(), 3, "{said:?}");
    for refused in &said[1..] {
        assert!(
            matches!(refused, Response::Error { code: ErrorCode::Forbidden, message, .. }
                if message.contains("inside the session of the agent it names")),
            "{refused:?}"
        );
    }
    assert!(kept(&daemon, &forger.id).is_empty());
    assert_eq!(kept(&daemon, &owner.id).len(), 1);
    let _ = owner_process.kill();
    let _ = forger_process.kill();
    let _ = owner_process.wait();
    let _ = forger_process.wait();
    serving.abort();
}
