//! Reviewers' verdicts on branches, each bound to the commit it read, and
//! the gate over them: documents of their own kind, by project. An agent
//! holding a role in the project, or the person, gives one; anybody may
//! ask the gate.
use super::*;
use agentdocker_core::gate::{
    self, CodeVerdict, Findings, GATE_ROLE, VERDICTS_PER_PROJECT, full_commit,
};
use agentdocker_core::{JournalEntry, JournalKind, SummarySource, Verdict};

pub(in crate::daemon) const VERDICT: &str = "code_verdict";

impl State {
    /// A project's verdicts, oldest first.
    fn project_verdicts(&mut self, project: &ProjectId) -> Result<Vec<CodeVerdict>, Box<Response>> {
        self.store_read("verdicts", |store| {
            store.documents_where::<CodeVerdict>(
                VERDICT,
                "project",
                project.as_str(),
                VERDICTS_PER_PROJECT + 1,
            )
        })
        .ok_or_else(|| {
            Box::new(self.storage_failure().unwrap_or_else(|| {
                Response::error(
                    ErrorCode::StorageUnavailable,
                    "the project's verdicts could not be read",
                )
            }))
        })
    }
}

impl Daemon {
    /// The project a verdict or a gate names, else the caller's own.
    async fn verdict_project(
        self: &Arc<Self>,
        agent: Option<&str>,
        project: Option<String>,
    ) -> Result<ProjectId, Box<Response>> {
        if let Some(selector) = project {
            return self.resolve_project(&selector).await;
        }
        let own = match agent {
            Some(agent) => lock(&self.state).actor(agent)?.project,
            None => None,
        };
        own.ok_or_else(|| invalid("name a project"))
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::daemon) async fn code_verdict(
        self: &Arc<Self>,
        agent: &str,
        project: Option<String>,
        branch: &str,
        head: &str,
        from: Option<String>,
        verdict: &str,
        findings: Findings,
        note: Option<String>,
    ) -> Response {
        // One spelling of each, checked and kept alike.
        let (branch, head) = (branch.trim(), head.trim());
        let Some(word) = Verdict::parse(verdict) else {
            return Response::error(
                ErrorCode::Invalid,
                "verdict must be approve, changes, or comment",
            );
        };
        // The person names the project their word is for: where their own
        // record happens to stand says nothing about which branch is meant.
        let named = project.is_some();
        let project = match self.verdict_project(Some(agent), project).await {
            Ok(project) => project,
            Err(response) => return *response,
        };
        let (actor, record) = {
            let mut state = lock(&self.state);
            let actor = match state.actor(agent) {
                Ok(actor) => actor,
                Err(response) => return *response,
            };
            let record = state
                .registry
                .get(&AgentId::from(actor.id.as_str()))
                .cloned();
            (actor, record)
        };
        if actor.human && !named {
            return Response::error(
                ErrorCode::Invalid,
                "the person names the project a verdict is for",
            );
        }
        let roles: Vec<String> = record
            .as_ref()
            .map(|r| r.roles().into_iter().map(str::to_owned).collect())
            .unwrap_or_default();
        if !actor.human {
            // An agent's word is its project's, given in a role the person
            // gave it, and never on its own branch.
            if actor.project.as_ref() != Some(&project) {
                return Response::error(
                    ErrorCode::Forbidden,
                    "an agent gives verdicts in its own project",
                );
            }
            if roles.is_empty() {
                return Response::error(
                    ErrorCode::Forbidden,
                    "a verdict is given by an agent holding a role in the project, or by the person",
                );
            }
            // The branch its checkout is on now, read from git, not what was
            // last reported.
            let Some(checkout) = record
                .as_ref()
                .and_then(|r| r.project.as_ref())
                .map(|p| p.dir().to_path_buf())
            else {
                return Response::error(
                    ErrorCode::Invalid,
                    "the reviewer has no checkout whose branch can be told",
                );
            };
            let read = tokio::task::spawn_blocking(move || checkout_branch(&checkout))
                .await
                .unwrap_or_else(|_| Err("the read did not complete".to_owned()));
            match read {
                Ok(Some(own)) if own == branch => {
                    return Response::error(
                        ErrorCode::Invalid,
                        "an agent does not review its own branch",
                    );
                }
                Ok(_) => {}
                Err(reason) => {
                    return Response::error(
                        ErrorCode::Unavailable,
                        format!("the reviewer's checkout branch could not be read: {reason}"),
                    );
                }
            }
        }
        let mut state = lock(&self.state);
        if let Some(error) = state.write_failure() {
            return error;
        }
        let existing = match state.project_verdicts(&project) {
            Ok(verdicts) => verdicts,
            Err(response) => return *response,
        };
        let at = Utc::now();
        let seq = existing.iter().map(|v| v.seq).max().unwrap_or(0) + 1;
        let given = CodeVerdict {
            id: gate::verdict_id(at),
            seq,
            project: project.clone(),
            branch: branch.to_owned(),
            head: head.to_owned(),
            from: from.map(|c| c.trim().to_owned()).filter(|c| !c.is_empty()),
            by: AgentId::from(actor.id.as_str()),
            by_name: record
                .as_ref()
                .map(|r| r.spec.name.clone())
                .unwrap_or_else(|| actor.id.clone()),
            human: actor.human,
            roles,
            verdict: word,
            findings,
            note: note.map(|n| n.trim().to_owned()).unwrap_or_default(),
            at,
        };
        if let Err(reason) = given.check() {
            return Response::error(ErrorCode::Invalid, reason);
        }
        // The oldest that stand for nobody go; a reviewer's standing word
        // never does, and a project full of standing words refuses more.
        let Some(forget) = gate::to_forget(&existing, &given, VERDICTS_PER_PROJECT) else {
            return Response::error(
                ErrorCode::Backpressure,
                "the project holds as many standing verdicts as it keeps; a reviewer's later word on a branch frees its earlier one",
            );
        };
        let forgotten: Vec<(&'static str, String)> =
            forget.into_iter().map(|id| (VERDICT, id)).collect();
        let kinds = vec![EventKind::CodeVerdictGiven {
            project: project.clone(),
            branch: given.branch.clone(),
            head: given.head.clone(),
            by: given.by.clone(),
            verdict: word,
        }];
        if let Some(error) = state.commit_change(
            "verdict",
            vec![doc(VERDICT, given.id.clone(), &given)],
            forgotten,
            kinds,
        ) {
            return error;
        }
        state.append_journal(journal_entry(&given));
        Response::CodeVerdict { verdict: given }
    }

    pub(in crate::daemon) async fn review_gate(
        self: &Arc<Self>,
        agent: Option<&str>,
        project: Option<String>,
        branch: &str,
        head: &str,
        role: Option<String>,
    ) -> Response {
        if !full_commit(head.trim()) {
            return Response::error(
                ErrorCode::Invalid,
                "the gate is asked about a full commit id (40 or 64 lowercase hex digits)",
            );
        }
        let project = match self.verdict_project(agent, project).await {
            Ok(project) => project,
            Err(response) => return *response,
        };
        let verdicts = match lock(&self.state).project_verdicts(&project) {
            Ok(verdicts) => verdicts,
            Err(response) => return *response,
        };
        let role = role
            .map(|r| r.trim().to_owned())
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| GATE_ROLE.to_owned());
        Response::ReviewGate {
            gate: gate::gate(&verdicts, branch.trim(), head.trim(), &role),
        }
    }
}

impl Daemon {
    /// A refusal when a verdict names an agent other than the process on
    /// the socket: the agent's own process, by pid and birth, must be that
    /// process or one of its parents, so no agent gives a reviewer's word.
    /// The person's word is the person check's.
    pub(crate) async fn verdict_author_refusal(
        self: &Arc<Self>,
        peer: Option<crate::daemon::person::Peer>,
        request: &Request,
    ) -> Option<Response> {
        let Request::CodeVerdict { agent, .. } = request else {
            return None;
        };
        if !self.person_check || peer.is_some_and(|(pid, _)| pid == std::process::id()) {
            return None;
        }
        let target = {
            let state = lock(&self.state);
            // A name nobody has is the handler's to answer.
            let id = state.registry.resolve(agent).ok()?;
            let record = state.registry.get(&id)?;
            if super::super::humans::is_human(record) {
                return None;
            }
            (record.pid, record.process_started_at)
        };
        let refuse = |why: &str| {
            Some(Response::error(
                ErrorCode::Forbidden,
                format!("a verdict is given from the reviewer's own session, and {why}"),
            ))
        };
        let (Some(pid), Some(born)) = target else {
            return refuse("this reviewer has no process to check it against");
        };
        let Some((sender, Some(sender_born))) = peer else {
            return refuse("the process that sent it cannot be identified");
        };
        if agentdocker_host::procinfo::start_time(sender) != Some(sender_born) {
            return refuse("the process that sent it has ended");
        }
        let Ok(Ok(table)) =
            tokio::task::spawn_blocking(agentdocker_host::procinfo::processes).await
        else {
            return refuse("its sender's parents could not be read");
        };
        let mut current = sender;
        for _ in 0..64 {
            if current == pid && agentdocker_host::procinfo::start_time(current) == Some(born) {
                return None;
            }
            let Some(process) = table.iter().find(|p| p.pid == current) else {
                break;
            };
            if process.ppid <= 1 || process.ppid == current {
                break;
            }
            current = process.ppid;
        }
        refuse("this was sent from outside it")
    }
}

/// The branch a checkout is on, from git: `None` when its HEAD is
/// detached, an error when git cannot say.
fn checkout_branch(checkout: &std::path::Path) -> Result<Option<String>, String> {
    let argv: Vec<String> = ["git", "symbolic-ref", "--quiet", "--short", "HEAD"]
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

/// The journal's line for a verdict: who said what of which branch, at
/// which commit, from which.
fn journal_entry(verdict: &CodeVerdict) -> JournalEntry {
    let short = |commit: &str| commit.chars().take(8).collect::<String>();
    let word = match verdict.verdict {
        Verdict::Approve => "approved",
        Verdict::Changes => "asked for changes on",
        Verdict::Comment => "commented on",
    };
    let range = match &verdict.from {
        Some(from) => format!("{}..{}", short(from), short(&verdict.head)),
        None => short(&verdict.head),
    };
    let Findings { p1, p2, p3 } = verdict.findings;
    let mut summary = format!(
        "{word} {} at {range} (P1 {p1}, P2 {p2}, P3 {p3})",
        verdict.branch
    );
    if !verdict.note.is_empty() {
        let note: String = verdict.note.chars().take(300).collect();
        summary.push_str(": ");
        summary.push_str(&note);
    }
    JournalEntry {
        project: verdict.project.clone(),
        seq: 0,
        at: verdict.at,
        agent: Some(verdict.by.clone()),
        agent_name: verdict.by_name.clone(),
        branch: Some(verdict.branch.clone()),
        checkout: None,
        worktree: None,
        kind: JournalKind::Review,
        summary,
        summary_source: SummarySource::Explicit,
        resources: Vec::new(),
        paths: Vec::new(),
        paths_total: 0,
        head_before: verdict.from.clone(),
        head_after: Some(verdict.head.clone()),
        changes: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    /// A checkout on `branch` (unborn: no commit is needed to be on it).
    fn repository(path: &std::path::Path, branch: &str) {
        std::fs::create_dir_all(path).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q", "-b", branch])
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn open(dir: &TempDir) -> Arc<Daemon> {
        let home = dir.path().to_path_buf();
        Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap())
    }

    async fn register(daemon: &Arc<Daemon>, name: &str, workdir: &std::path::Path) -> AgentRecord {
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

    fn verdict(
        agent: &str,
        project: Option<&str>,
        branch: &str,
        head: &str,
        word: &str,
    ) -> Request {
        Request::CodeVerdict {
            agent: agent.into(),
            project: project.map(str::to_owned),
            branch: branch.into(),
            head: head.into(),
            from: None,
            verdict: word.into(),
            findings: Findings::default(),
            note: Some("read it".into()),
        }
    }

    async fn ask(daemon: &Arc<Daemon>, agent: &str, head: &str) -> gate::Gate {
        match daemon
            .handle(Request::ReviewGate {
                agent: Some(agent.into()),
                project: None,
                branch: "feature/x".into(),
                head: head.into(),
                role: None,
            })
            .await
        {
            Response::ReviewGate { gate } => gate,
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

    fn refused(response: Response, code: ErrorCode) -> bool {
        matches!(response, Response::Error { code: c, .. } if c == code)
    }

    #[tokio::test]
    async fn only_an_approval_of_the_head_by_a_role_holder_or_the_person_opens_the_gate() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = open(&tmp);
        let repo = tmp.path().join("repo");
        repository(&repo, "main");
        let author = register(&daemon, "author", &repo).await;
        let reviewer = register(&daemon, "reviewer", &repo).await;
        let project = author.project.as_ref().unwrap().id().to_string();
        let (author, id) = (author.id.to_string(), reviewer.id.clone());
        let reviewer = id.to_string();

        // Without a role an agent's word is refused, and nothing is said.
        assert!(refused(
            daemon
                .handle(verdict(&reviewer, None, "feature/x", A, "approve"))
                .await,
            ErrorCode::Forbidden
        ));
        assert_eq!(ask(&daemon, &author, A).await, gate::Gate::Missing);

        give_role(&daemon, &id, "reviewer");
        // A short commit, a bad branch, or an approval carrying a P1 is
        // not a verdict.
        assert!(refused(
            daemon
                .handle(verdict(&reviewer, None, "feature/x", "abc1234", "approve"))
                .await,
            ErrorCode::Invalid
        ));
        assert!(refused(
            daemon
                .handle(verdict(&reviewer, None, "a..b", A, "approve"))
                .await,
            ErrorCode::Invalid
        ));
        let mut p1 = verdict(&reviewer, None, "feature/x", A, "approve");
        if let Request::CodeVerdict { findings, .. } = &mut p1 {
            findings.p1 = 1;
        }
        assert!(refused(daemon.handle(p1).await, ErrorCode::Invalid));

        let said = daemon
            .handle(verdict(&reviewer, None, "feature/x", A, "changes"))
            .await;
        assert!(matches!(said, Response::CodeVerdict { .. }), "{said:?}");
        assert!(matches!(
            ask(&daemon, &author, A).await,
            gate::Gate::Changes { .. }
        ));
        daemon
            .handle(verdict(&reviewer, None, "feature/x", A, "approve"))
            .await;
        assert!(ask(&daemon, &author, A).await.approved());
        // A commit since, a merge of main included, makes it stale.
        assert!(matches!(
            ask(&daemon, &author, B).await,
            gate::Gate::Stale { verdict } if verdict.head == A
        ));
        // The person's word counts without a role, in the project named.
        let said = daemon
            .handle(verdict(HUMAN, Some(&project), "feature/x", B, "approve"))
            .await;
        assert!(matches!(said, Response::CodeVerdict { .. }), "{said:?}");
        assert!(ask(&daemon, &author, B).await.approved());

        // The journal says each, against the branch and the commit read.
        let journal: Request = serde_json::from_value(serde_json::json!({
            "op": "journal", "project": project, "kind": "review", "limit": 50
        }))
        .unwrap();
        let Response::Journal { entries, .. } = daemon.handle(journal).await else {
            panic!("no journal");
        };
        let lines: Vec<&JournalEntry> = entries
            .iter()
            .filter(|e| e.branch.as_deref() == Some("feature/x"))
            .collect();
        assert_eq!(lines.len(), 3, "{entries:?}");
        assert!(lines.iter().any(|e| e.head_after.as_deref() == Some(B)
            && e.summary.starts_with("approved feature/x at bbbbbbbb")));
    }

    #[tokio::test]
    async fn an_agent_does_not_review_its_own_branch_or_another_project_s() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = open(&tmp);
        let repo = tmp.path().join("repo");
        let other = tmp.path().join("other");
        repository(&repo, "feature/mine");
        repository(&other, "main");
        let reviewer = register(&daemon, "reviewer", &repo).await;
        let elsewhere = register(&daemon, "elsewhere", &other).await;
        give_role(&daemon, &reviewer.id, "reviewer");
        let elsewhere_project = elsewhere.project.as_ref().unwrap().id().to_string();
        assert!(refused(
            daemon
                .handle(verdict(
                    reviewer.id.as_str(),
                    Some(&elsewhere_project),
                    "feature/x",
                    A,
                    "approve"
                ))
                .await,
            ErrorCode::Forbidden
        ));
        // Its checkout's branch as git says now, whatever was last
        // reported, and however the name is padded.
        lock(&daemon.state)
            .registry
            .get_mut(&reviewer.id)
            .unwrap()
            .vcs = Some(agentdocker_core::VcsState {
            branch: Some("elsewhere".into()),
            head: None,
            dirty: None,
            updated_at: Utc::now(),
        });
        for own in ["feature/mine", " feature/mine", "feature/mine\t"] {
            assert!(
                refused(
                    daemon
                        .handle(verdict(reviewer.id.as_str(), None, own, A, "approve"))
                        .await,
                    ErrorCode::Invalid
                ),
                "{own:?}"
            );
        }
        // Another branch is its to review.
        let said = daemon
            .handle(verdict(
                reviewer.id.as_str(),
                None,
                "feature/x",
                A,
                "approve",
            ))
            .await;
        assert!(matches!(said, Response::CodeVerdict { .. }), "{said:?}");
    }

    #[tokio::test]
    async fn the_person_names_the_project_and_an_unreadable_checkout_says_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = open(&tmp);
        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        // The person's record stands in a project; their word still names one.
        daemon
            .handle(Request::Me {
                workdir: Some(plain.clone()),
            })
            .await;
        assert!(refused(
            daemon
                .handle(verdict(HUMAN, None, "feature/x", A, "approve"))
                .await,
            ErrorCode::Invalid
        ));
        // A reviewer whose checkout git cannot read gives no word.
        let reviewer = register(&daemon, "reviewer", &plain).await;
        give_role(&daemon, &reviewer.id, "reviewer");
        assert!(refused(
            daemon
                .handle(verdict(
                    reviewer.id.as_str(),
                    None,
                    "feature/x",
                    A,
                    "approve"
                ))
                .await,
            ErrorCode::Unavailable
        ));
    }

    /// Over the real socket: a verdict naming a reviewer is refused unless
    /// the reviewer's own process sent it, or one of its parents did.
    #[cfg(unix)]
    #[tokio::test]
    async fn only_the_reviewer_s_own_session_gives_its_word() {
        use std::io::Write as _;
        let tmp = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::open(tmp.path().into(), tmp.path().join("s")).unwrap());
        let repo = tmp.path().join("repo");
        repository(&repo, "main");
        let listener = crate::server::bind(&daemon).await.unwrap();
        let serving = tokio::spawn(crate::server::serve(daemon.clone(), listener));
        let client = "import socket,sys\ns=socket.socket(socket.AF_UNIX)\ns.connect(sys.argv[1])\ns.sendall(sys.argv[2].encode()+b'\\n')\nprint(s.makefile().readline(),end='')\n";
        let spawn = || {
            std::process::Command::new("sh")
                .arg("-c")
                .arg("read request; exec python3 -c \"$0\" \"$1\" \"$request\"")
                .arg(client)
                .arg(&daemon.socket)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        };
        let send = |mut child: std::process::Child, request: String| async move {
            writeln!(child.stdin.take().unwrap(), "{request}").unwrap();
            let output = tokio::task::spawn_blocking(move || child.wait_with_output())
                .await
                .unwrap()
                .unwrap();
            serde_json::from_slice::<Response>(&output.stdout).unwrap()
        };
        let reviewer_process = spawn();
        let reviewer = match daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "reviewer".into(),
                    workdir: Some(repo.clone()),
                    ..AgentSpec::default()
                },
                pid: Some(reviewer_process.id()),
                session: None,
            })
            .await
        {
            Response::Agent { agent } => agent,
            other => panic!("{other:?}"),
        };
        give_role(&daemon, &reviewer.id, "reviewer");
        let request = serde_json::to_string(&verdict(
            reviewer.id.as_str(),
            None,
            "feature/x",
            A,
            "approve",
        ))
        .unwrap();
        // Another process saying it is the reviewer.
        let reply = send(spawn(), request.clone()).await;
        assert!(
            matches!(&reply, Response::Error { code: ErrorCode::Forbidden, message, .. }
                if message.contains("outside it")),
            "{reply:?}"
        );
        // The reviewer's own process.
        let reply = send(reviewer_process, request).await;
        assert!(matches!(reply, Response::CodeVerdict { .. }), "{reply:?}");
        serving.abort();
    }
}
