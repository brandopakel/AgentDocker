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
        let Some(word) = Verdict::parse(verdict) else {
            return Response::error(
                ErrorCode::Invalid,
                "verdict must be approve, changes, or comment",
            );
        };
        let project = match self.verdict_project(Some(agent), project).await {
            Ok(project) => project,
            Err(response) => return *response,
        };
        let mut state = lock(&self.state);
        let actor = match state.actor(agent) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        let record = state
            .registry
            .get(&AgentId::from(actor.id.as_str()))
            .cloned();
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
            if record
                .as_ref()
                .and_then(|r| r.vcs.as_ref())
                .and_then(|v| v.branch.as_deref())
                == Some(branch)
            {
                return Response::error(
                    ErrorCode::Invalid,
                    "an agent does not review its own branch",
                );
            }
        }
        if let Some(error) = state.write_failure() {
            return error;
        }
        let existing = match state.project_verdicts(&project) {
            Ok(verdicts) => verdicts,
            Err(response) => return *response,
        };
        let at = Utc::now();
        let given = CodeVerdict {
            id: gate::verdict_id(at),
            project: project.clone(),
            branch: branch.trim().to_owned(),
            head: head.trim().to_owned(),
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
        // The oldest go, so a project keeps its latest verdicts.
        let excess = (existing.len() + 1).saturating_sub(VERDICTS_PER_PROJECT);
        let forgotten: Vec<(&'static str, String)> = existing
            .iter()
            .take(excess)
            .map(|old| (VERDICT, old.id.clone()))
            .collect();
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
        std::fs::create_dir_all(&repo).unwrap();
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
        for dir in [&repo, &other] {
            std::fs::create_dir_all(dir).unwrap();
        }
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
        lock(&daemon.state)
            .registry
            .get_mut(&reviewer.id)
            .unwrap()
            .vcs = Some(agentdocker_core::VcsState {
            branch: Some("feature/mine".into()),
            head: None,
            dirty: None,
            updated_at: Utc::now(),
        });
        assert!(refused(
            daemon
                .handle(verdict(
                    reviewer.id.as_str(),
                    None,
                    "feature/mine",
                    A,
                    "approve"
                ))
                .await,
            ErrorCode::Invalid
        ));
    }
}
