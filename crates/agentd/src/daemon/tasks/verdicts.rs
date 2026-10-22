//! Reviewers' verdicts on branches, each bound to the commit it read, and
//! the gate over them: documents of their own kind, by project. An agent
//! holding a role in the project, or the person, gives one; anybody may
//! ask the gate. Beside them, what each agent has committed to, so that
//! no agent's word is taken on a branch it wrote.
use std::collections::HashMap;

use super::*;
use agentdocker_core::gate::{
    self, Authorship, CodeVerdict, Findings, GATE_ROLE, VERDICTS_PER_PROJECT, full_commit,
};
use agentdocker_core::{JournalEntry, JournalKind, SummarySource, Verdict};

pub(in crate::daemon) const VERDICT: &str = "code_verdict";
/// What an agent committed to: one document per project, agent and branch,
/// which neither journal pruning nor retention touches.
pub(in crate::daemon) const AUTHORSHIP: &str = "authorship";

impl State {
    /// The authorship a commit's journal entry proves, and the event that
    /// says so, when the entry names an agent (never the person) and the
    /// branch and commit it made: the record as it stands with this commit
    /// added. `None` when there is nothing to record or the record could
    /// not be read, which has then failed storage.
    pub(in crate::daemon) fn authorship_of(
        &mut self,
        entry: &JournalEntry,
    ) -> Option<((&'static str, String, String), EventKind)> {
        let agent = entry.agent.as_ref()?;
        let branch = entry.branch.as_deref().filter(|b| !b.is_empty())?;
        let commit = entry.head_after.as_deref()?;
        if self
            .registry
            .get(agent)
            .is_none_or(super::super::humans::is_human)
        {
            return None;
        }
        let id = gate::authorship_id(&entry.project, agent, branch);
        let previous = self.store_read("authorship", |store| {
            store.document::<Authorship>(AUTHORSHIP, &id)
        })?;
        let record = Authorship::after(
            previous
                .as_ref()
                .filter(|p| p.names(&entry.project, agent, branch)),
            &entry.project,
            agent,
            branch,
            commit,
            entry.at,
        );
        let kind = EventKind::AuthorshipRecorded {
            project: record.project.clone(),
            agent: record.agent.clone(),
            branch: record.branch.clone(),
            commit: record.last_commit.clone(),
        };
        Some((doc(AUTHORSHIP, id, &record), kind))
    }

    /// Whether `agent`, or a record folded into it, has committed to
    /// `branch` in `project`.
    fn has_committed_to(
        &mut self,
        project: &ProjectId,
        agent: &AgentId,
        branch: &str,
    ) -> Result<bool, Box<Response>> {
        let ids = self.registry.identity_ids(agent);
        self.store_read("authorship", |store| {
            for id in &ids {
                let key = gate::authorship_id(project, id, branch);
                if store
                    .document::<Authorship>(AUTHORSHIP, &key)?
                    .is_some_and(|a| a.names(project, id, branch))
                {
                    return Ok(true);
                }
            }
            Ok(false)
        })
        .ok_or_else(|| {
            Box::new(self.storage_failure().unwrap_or_else(|| {
                Response::error(
                    ErrorCode::StorageUnavailable,
                    "what the reviewer has committed to could not be read",
                )
            }))
        })
    }

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
                        ErrorCode::Forbidden,
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
        // What was checked before the branch was read must still hold: the
        // same record, in the same project, with the same roles.
        if !actor.human {
            let now = state.registry.get(&AgentId::from(actor.id.as_str()));
            let same = now.is_some_and(|now| {
                record.as_ref().is_some_and(|then| {
                    now.pid == then.pid
                        && now.process_started_at == then.process_started_at
                        && now.project.as_ref().map(ProjectRef::id) == Some(project.clone())
                        && now.status.is_live()
                }) && now
                    .roles()
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
                    == roles
            });
            if !same {
                return Response::error(
                    ErrorCode::Conflict,
                    "the reviewer changed while its verdict was checked; give it again",
                );
            }
        }
        if let Some(error) = state.write_failure() {
            return error;
        }
        // Nor on a branch it has committed to, wherever its checkout is now:
        // the daemon's own record of that, which outlives the journal.
        if !actor.human {
            match state.has_committed_to(&project, &AgentId::from(actor.id.as_str()), branch) {
                Ok(false) => {}
                Ok(true) => {
                    return Response::error(
                        ErrorCode::Forbidden,
                        "an agent does not review a branch it has committed to",
                    );
                }
                Err(response) => return *response,
            }
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

/// How a walk up from a verdict's sender ended.
#[derive(Debug, PartialEq, Eq)]
enum Author {
    Reviewer,
    Other(&'static str),
}

/// The dedicated Codex server `agentdocker codex` starts beside a
/// conversation's terminal: a Codex app-server listening on loopback, a
/// child of the same launcher as the reviewer's terminal, born after it,
/// where the launcher is the companion CLI's `codex` or `codex-native`.
fn dedicated_server(
    reviewer: &AgentRecord,
    server: &agentdocker_host::procinfo::Process,
    table: &[agentdocker_host::procinfo::Process],
    born: &impl Fn(u32) -> Option<DateTime<Utc>>,
    program: &impl Fn(u32) -> Option<std::path::PathBuf>,
    companion: &impl Fn(&std::path::Path) -> bool,
) -> bool {
    let Some(terminal) = reviewer
        .pid
        .and_then(|pid| table.iter().find(|p| p.pid == pid))
    else {
        return false;
    };
    let Some(launcher) = table.iter().find(|p| p.pid == server.ppid) else {
        return false;
    };
    let serving = agentdocker_host::procinfo::is_codex_binary(&server.argv)
        && server.argv.iter().any(|a| a == "app-server")
        && server
            .argv
            .windows(2)
            .any(|w| w[0] == "--listen" && w[1].starts_with("ws://127.0.0.1:"));
    // The launcher is this daemon's own companion CLI, by the file the
    // kernel says it runs; and the terminal speaks to this server, on the
    // loopback port the server listens on.
    let ours = matches!(
        launcher.argv.get(1).map(String::as_str),
        Some("codex" | "codex-native")
    ) && program(launcher.pid).is_some_and(|exe| companion(&exe));
    let port = |argv: &[String], flag: &str| {
        argv.windows(2)
            .find(|w| w[0] == flag && w[1].starts_with("ws://127.0.0.1:"))
            .map(|w| w[1].clone())
    };
    let paired = port(&server.argv, "--listen").is_some()
        && port(&server.argv, "--listen") == port(&terminal.argv, "--remote");
    let in_order = match (
        born(launcher.pid),
        born(server.pid),
        born(terminal.pid),
        reviewer.process_started_at,
    ) {
        (Some(l), Some(s), Some(t), Some(recorded)) => s >= l && t >= l && t == recorded,
        _ => false,
    };
    reviewer.spec.runtime == "codex"
        && terminal.ppid == server.ppid
        && serving
        && ours
        && paired
        && in_order
}

/// Walk up from `sender`, itself included, to the nearest session: a live
/// agent's own process (by pid and birth), or a provider's session process.
/// The reviewer's word is given only when that session is the reviewer's —
/// its own process, the server its supervised bridge started, or the
/// dedicated server `agentdocker codex` started beside it. Anything else,
/// an agent it started included, registered or not, is another session.
fn author(
    sender: u32,
    reviewer: &AgentRecord,
    agents: &HashMap<u32, (DateTime<Utc>, AgentId)>,
    table: &[agentdocker_host::procinfo::Process],
    born: impl Fn(u32) -> Option<DateTime<Utc>>,
    program: impl Fn(u32) -> Option<std::path::PathBuf>,
    companion: impl Fn(&std::path::Path) -> bool,
) -> Author {
    let mut current = sender;
    for _ in 0..64 {
        let Some(process) = table.iter().find(|p| p.pid == current) else {
            break;
        };
        let Some(birth) = born(current) else {
            // A process whose birth cannot be read could be anybody's.
            return Author::Other("its sender's parents could not all be read");
        };
        let birth = Some(birth);
        if let Some((recorded, owner)) = agents.get(&current)
            && birth == Some(*recorded)
        {
            return if *owner == reviewer.id {
                Author::Reviewer
            } else {
                Author::Other("it was sent from inside another agent's session")
            };
        }
        if agentdocker_host::procinfo::session_runtime_of(&process.argv).is_some()
            || agentdocker_host::procinfo::is_codex_binary(&process.argv)
        {
            let own = (reviewer.pid == Some(current)
                && birth.is_some()
                && birth == reviewer.process_started_at)
                || agentdocker_host::provider_input::owns_codex_process(reviewer, current, table)
                || dedicated_server(reviewer, process, table, &born, &program, &companion);
            return if own {
                Author::Reviewer
            } else {
                Author::Other("it was sent from inside a session that is not the reviewer's")
            };
        }
        if process.ppid <= 1 || process.ppid == current {
            break;
        }
        current = process.ppid;
    }
    Author::Other("this was sent from outside it")
}

impl Daemon {
    /// Authenticate a verdict's reviewer as the process on the socket's own
    /// session, and name it by its id from here on, so a rename while this
    /// runs cannot hand the word to another record. A refusal when it is
    /// not. The person's word is the person check's.
    pub(crate) async fn authenticate_verdict(
        self: &Arc<Self>,
        peer: Option<crate::daemon::person::Peer>,
        request: &mut Request,
    ) -> Option<Response> {
        let Request::CodeVerdict { agent, .. } = request else {
            return None;
        };
        let (reviewer, agents) = {
            let state = lock(&self.state);
            // Only a record named here reaches the handler, by its id: a name
            // that resolves to nobody now is refused, not looked up again
            // later when it might name somebody else.
            let id = match state.registry.resolve(agent) {
                Ok(id) => id,
                // The person by name, before they have a record: the person
                // check has already judged that claim, and the handler takes
                // the person as they are.
                Err(_) if agent == HUMAN => return None,
                Err(error) => return Some(registry_error(error)),
            };
            let Some(record) = state.registry.get(&id).cloned() else {
                return Some(Response::error(ErrorCode::NotFound, "agent vanished"));
            };
            let agents: HashMap<u32, (DateTime<Utc>, AgentId)> = state
                .registry
                .live()
                .filter(|r| !super::super::humans::is_human(r))
                .filter_map(|r| Some((r.pid?, (r.process_started_at?, r.id.clone()))))
                .collect();
            (record, agents)
        };
        *agent = reviewer.id.to_string();
        if super::super::humans::is_human(&reviewer)
            || !self.person_check
            || peer.is_some_and(|(pid, _)| pid == std::process::id())
        {
            return None;
        }
        let refuse = |why: &str| {
            Some(Response::error(
                ErrorCode::Forbidden,
                format!("a verdict is given from the reviewer's own session, and {why}"),
            ))
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
        match author(
            sender,
            &reviewer,
            &agents,
            &table,
            agentdocker_host::procinfo::start_time,
            |pid| agentdocker_host::procinfo::executable_path_of(pid).ok(),
            |exe| {
                crate::daemon::person::companion_cli()
                    .and_then(|cli| std::fs::canonicalize(cli).ok())
                    .zip(std::fs::canonicalize(exe).ok())
                    .is_some_and(|(cli, exe)| cli == exe)
            },
        ) {
            Author::Reviewer => None,
            Author::Other(why) => refuse(why),
        }
    }
}

/// The branch a checkout is on, from git: `None` when its HEAD is
/// detached or names something other than a branch, an error when git
/// cannot say.
fn checkout_branch(checkout: &std::path::Path) -> Result<Option<String>, String> {
    // The full ref, never `--short`: git lengthens a short name it finds
    // ambiguous (`heads/feature/mine` once a tag `feature/mine` exists),
    // which would let a reviewer dodge the own-branch refusal with a tag.
    let argv: Vec<String> = ["git", "symbolic-ref", "--quiet", "HEAD"]
        .map(str::to_owned)
        .to_vec();
    let output =
        agentdocker_host::command::run(checkout, &argv, std::time::Duration::from_secs(10))
            .map_err(|error| error.kind().to_string())?;
    if output.success {
        return Ok(output
            .stdout
            .trim()
            .strip_prefix("refs/heads/")
            .map(str::to_owned));
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
    #[test]
    fn a_tag_named_like_the_branch_does_not_hide_it() {
        let tmp = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.email=test@example.invalid",
                    "-c",
                    "user.name=Test",
                ])
                .args(args)
                .current_dir(tmp.path())
                .output()
                .unwrap();
            assert!(status.status.success(), "git {args:?}: {status:?}");
        };
        git(&["init", "-q"]);
        git(&["checkout", "-q", "-b", "feature/mine"]);
        git(&["commit", "-q", "--allow-empty", "-m", "first"]);
        git(&["tag", "feature/mine"]);
        assert_eq!(
            super::checkout_branch(tmp.path()).unwrap().as_deref(),
            Some("feature/mine")
        );
        git(&["checkout", "-q", "--detach"]);
        assert_eq!(super::checkout_branch(tmp.path()).unwrap(), None);
    }

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
                    ErrorCode::Forbidden
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

    /// Run git in `dir`, which must succeed; its standard output, trimmed.
    fn git_in(dir: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// A repository on `branch` with one commit and an identity of its own,
    /// so the daemon's commit works without the machine's.
    fn committed_repository(path: &std::path::Path, branch: &str) {
        std::fs::create_dir_all(path).unwrap();
        git_in(path, &["init", "-q", "-b", branch]);
        git_in(path, &["config", "user.name", "t"]);
        git_in(path, &["config", "user.email", "t@example.com"]);
        git_in(path, &["config", "commit.gpgsign", "false"]);
        git_in(path, &["commit", "-q", "--allow-empty", "-m", "root"]);
    }

    async fn commit_through_daemon(
        daemon: &Arc<Daemon>,
        agent: &AgentId,
        dir: &std::path::Path,
    ) -> (String, Option<String>) {
        let work = dir.join("work.txt");
        let mut text = std::fs::read_to_string(&work).unwrap_or_default();
        text.push_str("x\n");
        std::fs::write(&work, text).unwrap();
        git_in(dir, &["add", "."]);
        match daemon
            .handle(Request::Commit {
                agent: agent.to_string(),
                message: "mine".into(),
                all: false,
                push: false,
            })
            .await
        {
            Response::Committed { head, branch, .. } => (head, branch),
            other => panic!("{other:?}"),
        }
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

    fn refusal(response: Response) -> Option<(ErrorCode, String)> {
        match response {
            Response::Error { code, message, .. } => Some((code, message)),
            _ => None,
        }
    }

    /// The card's case: an agent commits through the daemon, the journal
    /// loses every line of it to a prune and to retention, its checkout
    /// moves to another branch, and its word on the branch it wrote is
    /// still refused. A tag named like the branch does not hide the branch
    /// from the commit. The person's word is unaffected.
    #[tokio::test]
    async fn a_branch_committed_to_stays_the_agent_s_after_the_journal_and_its_checkout_move_on() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = open(&tmp);
        let repo = tmp.path().join("repo");
        committed_repository(&repo, "feature/mine");
        git_in(&repo, &["tag", "feature/mine"]);
        let reviewer = register(&daemon, "reviewer", &repo).await;
        give_role(&daemon, &reviewer.id, "reviewer");
        let project = reviewer.project.as_ref().unwrap().id();

        let (first, branch) = commit_through_daemon(&daemon, &reviewer.id, &repo).await;
        assert_eq!(
            branch.as_deref(),
            Some("feature/mine"),
            "not heads/feature/mine"
        );
        let (second, _) = commit_through_daemon(&daemon, &reviewer.id, &repo).await;
        let commits = commit_entries(&daemon, &project).await;
        assert_eq!(commits.len(), 2, "{commits:?}");
        assert!(
            commits
                .iter()
                .all(|e| e.branch.as_deref() == Some("feature/mine"))
        );
        let recorded: Vec<String> = daemon
            .recent_events(50)
            .into_iter()
            .filter_map(|e| match e.kind {
                EventKind::AuthorshipRecorded {
                    agent,
                    branch,
                    commit,
                    ..
                } if agent == reviewer.id && branch == "feature/mine" => Some(commit),
                _ => None,
            })
            .collect();
        assert_eq!(recorded, vec![first.clone(), second.clone()]);
        let id = gate::authorship_id(&project, &reviewer.id, "feature/mine");
        let kept: Authorship = lock(&daemon.state)
            .store
            .document(AUTHORSHIP, &id)
            .unwrap()
            .unwrap();
        assert_eq!(kept.last_commit, second, "the latest commit");
        assert!(kept.first_seen <= kept.last_seen);

        // The journal forgets: the first commit by request, the second by
        // retention once it is older than the window.
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
                state.store.age_journal_for_test(
                    &project,
                    seq,
                    Utc::now() - chrono::Duration::days(40),
                );
            }
        }
        std::fs::write(
            daemon.home.join("agentd.toml"),
            "[journal]\nretention = \"20d\"\n",
        )
        .unwrap();
        daemon.apply_journal_retention();
        assert!(
            commit_entries(&daemon, &project).await.is_empty(),
            "the journal kept nothing"
        );

        // The checkout moves on; the branch it wrote is still its own.
        git_in(&repo, &["checkout", "-q", "-b", "elsewhere"]);
        let (code, message) = refusal(
            daemon
                .handle(verdict(
                    reviewer.id.as_str(),
                    None,
                    "feature/mine",
                    A,
                    "approve",
                ))
                .await,
        )
        .expect("refused");
        assert_eq!(code, ErrorCode::Forbidden);
        assert!(message.contains("committed to"), "{message}");
        // Nothing was said on it, and other branches are its to review.
        assert_eq!(
            lock(&daemon.state)
                .project_verdicts(&project)
                .unwrap()
                .len(),
            0
        );
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
        // The person's word on it counts as ever.
        let said = daemon
            .handle(verdict(
                HUMAN,
                Some(project.as_str()),
                "feature/mine",
                A,
                "approve",
            ))
            .await;
        assert!(matches!(said, Response::CodeVerdict { .. }), "{said:?}");
    }

    /// A reviewer whose checkout only goes to a branch — switched onto it,
    /// brought up to date with a fast-forward that has the old head as its
    /// parent just as a commit would — reviews it: the journal names the
    /// reviewer on those moves, and that is not authorship. A commit it
    /// makes there with git is, after it has switched away too.
    #[tokio::test]
    async fn checking_a_branch_out_is_not_writing_it_and_committing_there_is() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = open(&tmp);
        let repo = tmp.path().join("repo");
        committed_repository(&repo, "main");
        let tree = git_in(&repo, &["rev-parse", "HEAD^{tree}"]);
        let root = git_in(&repo, &["rev-parse", "HEAD"]);
        // Somebody else's branch, and a commit of theirs on top of it, made
        // without touching this checkout.
        let x1 = git_in(&repo, &["commit-tree", &tree, "-p", &root, "-m", "x1"]);
        git_in(&repo, &["branch", "feature/x", &x1]);
        let x2 = git_in(&repo, &["commit-tree", &tree, "-p", &x1, "-m", "x2"]);
        git_in(&repo, &["branch", "upstream", &x2]);
        let reviewer = register(&daemon, "reviewer", &repo).await;
        give_role(&daemon, &reviewer.id, "reviewer");
        let project = reviewer.project.as_ref().unwrap().id();
        daemon.refresh_vcs(None).await;

        git_in(&repo, &["checkout", "-q", "feature/x"]);
        daemon.refresh_vcs(None).await;
        git_in(&repo, &["merge", "-q", "--ff-only", "upstream"]);
        daemon.refresh_vcs(None).await;
        git_in(&repo, &["checkout", "-q", "main"]);
        daemon.refresh_vcs(None).await;
        let moves = commit_entries(&daemon, &project).await;
        assert!(
            moves.iter().any(|e| e.agent.as_ref() == Some(&reviewer.id)
                && e.branch.as_deref() == Some("feature/x")
                && e.head_before.as_deref() == Some(x1.as_str())
                && e.head_after.as_deref() == Some(x2.as_str())
                && e.summary.starts_with("committed ")),
            "the journal names the reviewer on the fast-forward: {moves:?}"
        );
        assert!(
            !daemon
                .recent_events(100)
                .iter()
                .any(|e| matches!(e.kind, EventKind::AuthorshipRecorded { .. })),
            "nothing was authored"
        );
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

        // A commit made there with git, seen by the watcher, is authorship.
        git_in(&repo, &["checkout", "-q", "feature/x"]);
        daemon.refresh_vcs(None).await;
        git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "mine"]);
        let mine = git_in(&repo, &["rev-parse", "HEAD"]);
        daemon.refresh_vcs(None).await;
        git_in(&repo, &["checkout", "-q", "main"]);
        daemon.refresh_vcs(None).await;
        assert!(daemon.recent_events(100).iter().any(|e| matches!(&e.kind,
            EventKind::AuthorshipRecorded { agent, branch, commit, .. }
            if *agent == reviewer.id && branch == "feature/x" && *commit == mine)));
        let (code, _) = refusal(
            daemon
                .handle(verdict(
                    reviewer.id.as_str(),
                    None,
                    "feature/x",
                    B,
                    "approve",
                ))
                .await,
        )
        .expect("refused");
        assert_eq!(code, ErrorCode::Forbidden);
    }

    /// An authorship under a record folded into the reviewer's counts for
    /// the reviewer; one that cannot be read refuses the word rather than
    /// passing it.
    #[tokio::test]
    async fn a_folded_record_s_commits_are_the_reviewer_s_and_an_unreadable_record_refuses() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = open(&tmp);
        let repo = tmp.path().join("repo");
        committed_repository(&repo, "main");
        let earlier_dir = tmp.path().join("earlier");
        git_in(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feature/fold",
                earlier_dir.to_str().unwrap(),
            ],
        );
        let earlier = register(&daemon, "earlier", &earlier_dir).await;
        let reviewer = register(&daemon, "reviewer", &repo).await;
        give_role(&daemon, &reviewer.id, "reviewer");
        let project = reviewer.project.as_ref().unwrap().id();
        assert_eq!(earlier.project.as_ref().unwrap().id(), project);
        commit_through_daemon(&daemon, &earlier.id, &earlier_dir).await;
        let said = daemon
            .handle(verdict(
                reviewer.id.as_str(),
                None,
                "feature/fold",
                A,
                "comment",
            ))
            .await;
        assert!(
            matches!(said, Response::CodeVerdict { .. }),
            "not yet its own: {said:?}"
        );

        lock(&daemon.state)
            .registry
            .fold_into(std::slice::from_ref(&earlier.id), &reviewer.id)
            .unwrap();
        assert!(refused(
            daemon
                .handle(verdict(
                    reviewer.id.as_str(),
                    None,
                    "feature/fold",
                    A,
                    "approve"
                ))
                .await,
            ErrorCode::Forbidden
        ));

        let broken = gate::authorship_id(&project, &reviewer.id, "feature/broken");
        lock(&daemon.state)
            .store
            .put_document(AUTHORSHIP, &broken, &"not an authorship")
            .unwrap();
        assert!(refused(
            daemon
                .handle(verdict(
                    reviewer.id.as_str(),
                    None,
                    "feature/broken",
                    A,
                    "approve"
                ))
                .await,
            ErrorCode::StorageUnavailable
        ));
    }

    fn row(pid: u32, ppid: u32, argv: &[&str]) -> agentdocker_host::procinfo::Process {
        agentdocker_host::procinfo::Process {
            pid,
            ppid,
            argv: argv.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    fn reviewer_record(pid: u32, runtime: &str) -> AgentRecord {
        let mut record = AgentRecord::new(
            AgentSpec {
                name: "reviewer".into(),
                runtime: runtime.into(),
                ..AgentSpec::default()
            },
            false,
            Utc::now(),
        );
        record.pid = Some(pid);
        record.process_started_at = DateTime::from_timestamp(1_700_000_000 + i64::from(pid), 0);
        record
    }

    /// Every topology a verdict comes from, on a table: the reviewer's own
    /// Claude session; an agent it started, registered or not; the dedicated
    /// server `agentdocker codex` starts beside its terminal; a look-alike
    /// whose launcher is not ours; and nothing at all.
    #[test]
    fn a_verdict_is_the_reviewer_s_only_from_the_nearest_session_being_its_own() {
        // 40's birth cannot be read.
        let born = |pid: u32| {
            (pid != 40).then(|| DateTime::from_timestamp(1_700_000_000 + i64::from(pid), 0))?
        };
        // 500 runs another program also called agentdocker.
        let ours = |pid: u32| match pid {
            300 | 600 => Some(std::path::PathBuf::from("/opt/agentdocker")),
            500 => Some(std::path::PathBuf::from("/tmp/agentdocker")),
            _ => None,
        };
        let table = [
            // A Claude reviewer (10) and what runs under it.
            row(10, 1, &["/usr/local/bin/claude", "--resume", "x"]),
            row(
                11,
                10,
                &["/opt/agentdocker", "mcp", "--runtime", "claude-code"],
            ),
            // A Claude it started, registered (20) and not (30).
            row(20, 10, &["/usr/local/bin/claude", "-p", "go"]),
            row(21, 20, &["python3", "client.py"]),
            row(30, 10, &["/usr/local/bin/claude", "-p", "go"]),
            row(31, 30, &["python3", "client.py"]),
            // `agentdocker codex`: launcher 300, terminal 301, server 302.
            row(
                300,
                5,
                &["/opt/agentdocker", "codex-native", "--program", "/x/codex"],
            ),
            row(301, 300, &["/x/codex", "--remote", "ws://127.0.0.1:4100"]),
            row(
                302,
                300,
                &["/x/codex", "app-server", "--listen", "ws://127.0.0.1:4100"],
            ),
            row(303, 302, &["/opt/agentdocker", "mcp", "--runtime", "codex"]),
            // The same shape under a launcher that is not ours (500).
            row(500, 5, &["/tmp/fake", "codex-native"]),
            row(501, 500, &["/x/codex", "--remote", "ws://127.0.0.1:4200"]),
            row(
                502,
                500,
                &["/x/codex", "app-server", "--listen", "ws://127.0.0.1:4200"],
            ),
            row(503, 502, &["/opt/agentdocker", "mcp"]),
            // Our launcher, but its server is not the terminal's (600).
            row(600, 5, &["/opt/agentdocker", "codex-native"]),
            row(601, 600, &["/x/codex", "--remote", "ws://127.0.0.1:4300"]),
            row(
                602,
                600,
                &["/x/codex", "app-server", "--listen", "ws://127.0.0.1:4999"],
            ),
            row(603, 602, &["/opt/agentdocker", "mcp"]),
            // A custom agent the reviewer started, whose birth is unreadable.
            row(40, 10, &["/opt/tools/custom-agent"]),
            row(41, 40, &["python3", "client.py"]),
            row(5, 1, &["-zsh"]),
            row(6, 5, &["python3", "client.py"]),
        ];
        let claude = reviewer_record(10, "claude-code");
        let nested = reviewer_record(20, "claude-code");
        let agents: HashMap<u32, (DateTime<Utc>, AgentId)> = [
            (10, (born(10).unwrap(), claude.id.clone())),
            (20, (born(20).unwrap(), nested.id.clone())),
        ]
        .into();
        let companion = |exe: &std::path::Path| exe == std::path::Path::new("/opt/agentdocker");
        let go =
            |sender, reviewer: &AgentRecord, agents: &HashMap<u32, (DateTime<Utc>, AgentId)>| {
                author(sender, reviewer, agents, &table, born, ours, companion)
            };
        assert_eq!(go(11, &claude, &agents), Author::Reviewer);
        assert!(
            matches!(go(21, &claude, &agents), Author::Other(why) if why.contains("another agent's"))
        );
        assert!(
            matches!(go(31, &claude, &agents), Author::Other(why) if why.contains("not the reviewer's")),
            "an unregistered nested session"
        );
        let native = reviewer_record(301, "codex");
        let native_agents: HashMap<u32, (DateTime<Utc>, AgentId)> =
            [(301, (born(301).unwrap(), native.id.clone()))].into();
        assert_eq!(
            go(303, &native, &native_agents),
            Author::Reviewer,
            "its dedicated server"
        );
        let fake = reviewer_record(501, "codex");
        let fake_agents: HashMap<u32, (DateTime<Utc>, AgentId)> =
            [(501, (born(501).unwrap(), fake.id.clone()))].into();
        assert!(
            matches!(go(503, &fake, &fake_agents), Author::Other(_)),
            "a launcher that is not ours"
        );
        assert!(
            matches!(go(303, &claude, &agents), Author::Other(_)),
            "another reviewer's server"
        );
        assert!(matches!(go(6, &claude, &agents), Author::Other(why) if why.contains("outside")));
        let unpaired = reviewer_record(601, "codex");
        let unpaired_agents: HashMap<u32, (DateTime<Utc>, AgentId)> =
            [(601, (born(601).unwrap(), unpaired.id.clone()))].into();
        assert!(
            matches!(go(603, &unpaired, &unpaired_agents), Author::Other(_)),
            "a sibling server the terminal does not speak to"
        );
        let mut with_custom = agents.clone();
        with_custom.insert(
            40,
            (
                DateTime::from_timestamp(1_700_000_040, 0).unwrap(),
                reviewer_record(40, "custom").id,
            ),
        );
        assert!(
            matches!(go(41, &claude, &with_custom), Author::Other(why) if why.contains("could not all be read")),
            "an unreadable birth stops the walk"
        );
    }

    /// The reviewer is named by its id once it is authenticated, so a name
    /// handed to another record while the check ran changes nothing.
    #[tokio::test]
    async fn an_authenticated_verdict_names_its_reviewer_by_id() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = open(&tmp);
        let repo = tmp.path().join("repo");
        repository(&repo, "main");
        let reviewer = register(&daemon, "reviewer", &repo).await;
        let mut request = verdict("reviewer", None, "feature/x", A, "approve");
        let own = Some((
            std::process::id(),
            agentdocker_host::procinfo::start_time(std::process::id()),
        ));
        assert!(
            daemon
                .authenticate_verdict(own, &mut request)
                .await
                .is_none()
        );
        let Request::CodeVerdict { agent, .. } = &request else {
            unreachable!()
        };
        assert_eq!(agent, &reviewer.id.to_string());
        // The person by name, with no record yet, goes on as the person.
        let mut person = verdict(HUMAN, None, "feature/x", A, "approve");
        assert!(
            daemon
                .authenticate_verdict(own, &mut person)
                .await
                .is_none()
        );
        let Request::CodeVerdict { agent, .. } = &person else {
            unreachable!()
        };
        assert_eq!(agent, HUMAN);
        // A name nobody has now is refused here, never looked up later.
        let mut unknown = verdict("unused-name", None, "feature/x", A, "approve");
        assert!(matches!(
            daemon.authenticate_verdict(own, &mut unknown).await,
            Some(Response::Error {
                code: ErrorCode::NotFound,
                ..
            })
        ));
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
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
        // Another process saying it is the reviewer: refused, whichever
        // session (if any) it runs under where the tests do.
        let reply = send(spawn(), request.clone()).await;
        assert!(
            matches!(
                &reply,
                Response::Error {
                    code: ErrorCode::Forbidden,
                    ..
                }
            ),
            "{reply:?}"
        );
        // An agent the reviewer started, registered as itself, saying it is
        // the reviewer: its own session is the nearest, so it is refused.
        {
            use std::io::BufRead as _;
            let inner = tmp.path().join("inner.sh");
            let asked = tmp.path().join("asked");
            std::fs::write(
                &inner,
                "#!/bin/sh\nwhile [ ! -f \"$ASKED\" ]; do sleep 0.05; done\nexec python3 -c \"$CLIENT\" \"$SOCK\" \"$(cat \"$ASKED\")\"\n",
            )
            .unwrap();
            let mut nesting = std::process::Command::new("sh")
                .arg("-c")
                .arg("sh \"$0\" & echo $!; wait")
                .arg(&inner)
                .env("ASKED", &asked)
                .env("CLIENT", client)
                .env("SOCK", &daemon.socket)
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let mut out = std::io::BufReader::new(nesting.stdout.take().unwrap());
            let mut line = String::new();
            out.read_line(&mut line).unwrap();
            let nested: u32 = line.trim().parse().unwrap();
            let outer = match daemon
                .handle(Request::Register {
                    spec: AgentSpec {
                        name: "outer-reviewer".into(),
                        workdir: Some(repo.clone()),
                        ..AgentSpec::default()
                    },
                    pid: Some(nesting.id()),
                    session: None,
                })
                .await
            {
                Response::Agent { agent } => agent,
                other => panic!("{other:?}"),
            };
            give_role(&daemon, &outer.id, "reviewer");
            let registered = daemon
                .handle(Request::Register {
                    spec: AgentSpec {
                        name: "nested".into(),
                        workdir: Some(repo.clone()),
                        ..AgentSpec::default()
                    },
                    pid: Some(nested),
                    session: None,
                })
                .await;
            assert!(
                matches!(registered, Response::Agent { .. }),
                "{registered:?}"
            );
            let request =
                serde_json::to_string(&verdict(outer.id.as_str(), None, "feature/y", A, "approve"))
                    .unwrap();
            std::fs::write(&asked, request).unwrap();
            let mut reply = String::new();
            out.read_line(&mut reply).unwrap();
            let reply: Response = serde_json::from_str(&reply).unwrap();
            assert!(
                matches!(&reply, Response::Error { code: ErrorCode::Forbidden, message, .. }
                    if message.contains("another agent's session")),
                "{reply:?}"
            );
            let _ = nesting.wait();
        }
        // The reviewer's own process.
        let reply = send(reviewer_process, request).await;
        assert!(matches!(reply, Response::CodeVerdict { .. }), "{reply:?}");
        serving.abort();
    }
}
