//! Reviewers' verdicts on branches, each bound to the commit it read, and
//! the gate over them: documents of their own kind, by project. An agent
//! holding a role in the project, or the person, gives one; anybody may
//! ask the gate. No agent's word is taken on the branch its checkout is
//! on, on a branch it holds (`branch:<name>` lease), on a branch the
//! daemon saw it commit to (`daemon/authorship.rs`), nor on one the
//! journal says it committed to. A request for changes whose reviewer
//! has gone is set aside at a role's gate once somebody holding the role
//! passes that commit or a later one, as git says, and the person can set
//! any verdict aside; either is kept on the verdict.
use std::collections::{BTreeSet, HashMap};

use super::*;
use agentdocker_core::gate::{
    self, CodeVerdict, Findings, GATE_ROLE, Lifted, NOTE_CHARS, SupersedeReason, Supersession,
    VERDICTS_PER_PROJECT, Word, full_commit,
};
use agentdocker_core::{HUMAN_RUNTIME, JournalEntry, JournalKind, SummarySource};

pub(in crate::daemon) const VERDICT: &str = "code_verdict";

impl State {
    /// Whether the journal holds a commit on `branch` in `project`
    /// attributed to `agent` or an id folded into it: the daemon's own
    /// `commit`, a witnessed worktree commit, or the watcher's attribution
    /// of one to the only agent in the checkout or the `branch:` holder.
    /// Best effort by design: pruning and retention empty the journal, and
    /// `authorship` is the record that outlives it.
    fn journal_says_committed(
        &mut self,
        project: &ProjectId,
        agent: &AgentId,
        branch: &str,
    ) -> Result<bool, Box<Response>> {
        for id in self.registry.identity_ids(agent) {
            let query = crate::store::JournalQuery {
                agent: Some(id),
                branch: Some(branch.to_owned()),
                kind: Some(JournalKind::Commit),
                ..crate::store::JournalQuery::new(project.clone(), 1)
            };
            let found = self
                .store_read("journal", |store| store.journal(&query))
                .ok_or_else(|| {
                    Box::new(self.storage_failure().unwrap_or_else(|| {
                        Response::error(
                            ErrorCode::StorageUnavailable,
                            "what the journal says the reviewer committed could not be read",
                        )
                    }))
                })?;
            if !found.is_empty() {
                return Ok(true);
            }
        }
        Ok(false)
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

    /// What the registry says of the agents whose verdicts are on
    /// `branch`, for `role`'s gate: whose record has ended — exited,
    /// failed or forgotten, under its id or one it was folded into — and
    /// who is live in the project holding the role now and has not
    /// committed to the branch.
    fn succession(
        &mut self,
        verdicts: &[CodeVerdict],
        branch: &str,
        role: &str,
        project: &ProjectId,
    ) -> gate::Succession {
        let mut succession = gate::Succession::default();
        let by: BTreeSet<AgentId> = verdicts
            .iter()
            .filter(|v| v.branch == branch && !v.human)
            .map(|v| v.by.clone())
            .collect();
        let mut live = Vec::new();
        for id in by {
            match self.registry.get(self.registry.canonical_id(&id)) {
                Some(record) if record.status.is_live() => live.push((id, record.clone())),
                _ => {
                    succession.ended.insert(id);
                }
            }
        }
        // Who holds the role matters only to a departed reviewer's word.
        if succession.ended.is_empty() {
            return succession;
        }
        for (id, record) in live {
            let holds = record.project.as_ref().map(ProjectRef::id).as_ref() == Some(project)
                && record.roles().contains(&role);
            // An author speaks for nobody's review of its branch, a commit
            // it made after its verdict included; what cannot be read is no.
            if holds && matches!(self.has_authored(project, &id, branch), Ok(false)) {
                succession.holders.insert(id);
            }
        }
        succession
    }

    /// The repository a project's records name, to ask git about its
    /// commits.
    fn project_root(&self, project: &ProjectId) -> Option<std::path::PathBuf> {
        self.registry
            .all()
            .filter_map(|r| r.project.as_ref())
            .find(|p| p.id() == *project)
            .map(|p| p.root.clone())
    }

    /// Keep the requests for changes a gate set aside, once each. While
    /// coordination is being handed over nothing is kept and nothing
    /// refused: the gate's answer is the same, and the next ask finds the
    /// lift again. A storage failure is the asker's error, as for any
    /// write.
    fn keep_lifted(&mut self, lifted: &[Lifted]) -> Option<Response> {
        if lifted.is_empty() || self.fenced() {
            return None;
        }
        if let Some(error) = self.write_failure() {
            return Some(error);
        }
        let marked: Vec<_> = lifted
            .iter()
            .map(|l| (&l.verdict, &l.mark, Some(&l.pass)))
            .collect();
        self.commit_superseded(&marked)
    }

    /// Write verdicts as marked with all that says so — each one's
    /// `code_verdict_superseded` event and its journal line — in one
    /// commit: none of it lands without the rest, and a failure is the
    /// caller's to return.
    fn commit_superseded(
        &mut self,
        marked: &[(&CodeVerdict, &Supersession, Option<&CodeVerdict>)],
    ) -> Option<Response> {
        let documents: Vec<Doc> = marked
            .iter()
            .map(|(verdict, _, _)| doc(VERDICT, verdict.id.clone(), *verdict))
            .collect();
        let entries: Vec<JournalEntry> = marked
            .iter()
            .map(|(verdict, mark, pass)| {
                let mut entry = superseded_entry(verdict, mark, *pass);
                entry.seq = self.next_journal_seq(&entry.project);
                entry
            })
            .collect();
        let now = Utc::now();
        let events: Vec<Event> = marked
            .iter()
            .map(|(verdict, mark, _)| superseded_event(verdict, mark))
            .chain(entries.iter().map(|entry| EventKind::JournalAppended {
                entry: entry.clone(),
            }))
            .map(|kind| Event::new(kind, now))
            .collect();
        let cached = entries.clone();
        let committed = self.commit(
            "verdict superseded",
            events,
            |store, events| store.documents_with_journal(&documents, &entries, events),
            |state, _, ()| {
                for entry in cached {
                    state.cache_journal(entry);
                }
            },
        );
        if committed != Persisted::Committed {
            return Some(self.write_failure().unwrap_or_else(|| {
                Response::error(
                    ErrorCode::StorageUnavailable,
                    "the verdict set aside was not recorded",
                )
            }));
        }
        None
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
        own.ok_or_else(|| Box::new(Response::invalid("name a project")))
    }

    // Kept as the request spells it while the review work in flight changes this signature.
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
        let Some(word) = Word::parse(verdict) else {
            return Response::error(ErrorCode::Invalid, "verdict must be pass, changes, or none");
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
        // Nor on a branch it holds: the `branch:` lease says whose the
        // branch is now, and the author may not pass its own change.
        if !actor.human {
            state.expire_leases();
            let ids = state
                .registry
                .identity_ids(&AgentId::from(actor.id.as_str()));
            let key = ResourceKey::new(format!("branch:{branch}"));
            if let Some(lease) = state
                .leases
                .holders_of(&key)
                .into_iter()
                .find(|lease| ids.contains(&lease.holder))
            {
                return Response::error(
                    ErrorCode::Forbidden,
                    format!(
                        "an agent does not review a branch it holds: lease {} on {}",
                        lease.id,
                        lease.resource.value()
                    ),
                );
            }
        }
        // Nor on a branch it has committed to, wherever its checkout is now:
        // what the daemon made or witnessed it commit, read by the branch's
        // full ref, which the journal's pruning and retention never touch;
        // and what the journal attributes to it as a commit on the branch,
        // for as long as the journal keeps it.
        if !actor.human {
            match state.has_authored(&project, &AgentId::from(actor.id.as_str()), branch) {
                Ok(false) => {}
                Ok(true) => {
                    return Response::error(
                        ErrorCode::Forbidden,
                        "an agent does not review a branch it has committed to",
                    );
                }
                Err(response) => return *response,
            }
            match state.journal_says_committed(&project, &AgentId::from(actor.id.as_str()), branch)
            {
                Ok(false) => {}
                Ok(true) => {
                    return Response::error(
                        ErrorCode::Forbidden,
                        "an agent does not review a branch the journal says it committed to",
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
            // The person's word is the person's, whatever record it came
            // through; an agent's names the runtime it registered as.
            runtime: if actor.human {
                HUMAN_RUNTIME.to_owned()
            } else {
                record
                    .as_ref()
                    .map(|r| r.spec.runtime.clone())
                    .unwrap_or_default()
            },
            verdict: word,
            findings,
            note: note.map(|n| n.trim().to_owned()).unwrap_or_default(),
            at,
            superseded: Vec::new(),
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
            findings: given.findings,
            runtime: given.runtime.clone(),
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
        let role = role
            .map(|r| r.trim().to_owned())
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| GATE_ROLE.to_owned());
        let (branch, head) = (branch.trim(), head.trim());
        // Whether a successor's pass read a commit at or after a departed
        // reviewer's request for changes is git's to say: asked once, with
        // no lock held, and only when there is such a pass of another
        // commit. A pair that appears while git is asked stays unknown, so
        // lifts nothing until the next ask.
        let mut descends = BTreeSet::new();
        let mut asked = false;
        loop {
            let (root, questions) = {
                let mut state = lock(&self.state);
                let verdicts = match state.project_verdicts(&project) {
                    Ok(verdicts) => verdicts,
                    Err(response) => return *response,
                };
                let mut succession = state.succession(&verdicts, branch, &role, &project);
                let questions = gate::ancestry_questions(&verdicts, branch, &role, &succession);
                match state.project_root(&project) {
                    Some(root) if !asked && !questions.is_empty() => (root, questions),
                    _ => {
                        succession.descends = descends;
                        let gated =
                            gate::gated(&verdicts, branch, head, &role, &succession, Utc::now());
                        if let Some(error) = state.keep_lifted(&gated.lifted) {
                            return error;
                        }
                        return Response::ReviewGate {
                            gate: gated.gate,
                            superseded: gated.superseded,
                        };
                    }
                }
            };
            asked = true;
            descends = tokio::task::spawn_blocking(move || {
                questions
                    .into_iter()
                    .filter(|(earlier, later)| at_or_after(&root, earlier, later))
                    .collect()
            })
            .await
            .unwrap_or_default();
        }
    }

    /// The person sets a verdict aside; an agent may not.
    pub(in crate::daemon) async fn supersede_verdict(
        self: &Arc<Self>,
        agent: &str,
        project: Option<String>,
        verdict: &str,
        reason: Option<String>,
    ) -> Response {
        let named = project.is_some();
        let project = match self.verdict_project(Some(agent), project).await {
            Ok(project) => project,
            Err(response) => return *response,
        };
        let note = reason.map(|r| r.trim().to_owned()).unwrap_or_default();
        if note.chars().count() > NOTE_CHARS {
            return Response::error(ErrorCode::Invalid, "the reason is at most 4000 characters");
        }
        let mut state = lock(&self.state);
        let actor = match state.actor(agent) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        if !actor.human {
            return Response::error(
                ErrorCode::Forbidden,
                "setting a verdict aside is the person's to do",
            );
        }
        if !named {
            return Response::error(
                ErrorCode::Invalid,
                "the person names the project a verdict is in",
            );
        }
        if let Some(error) = state.write_failure() {
            return error;
        }
        let verdicts = match state.project_verdicts(&project) {
            Ok(verdicts) => verdicts,
            Err(response) => return *response,
        };
        let id = verdict.trim();
        let Some(mut found) = verdicts.into_iter().find(|v| v.id == id) else {
            return Response::error(
                ErrorCode::NotFound,
                format!("no verdict {id} in the project"),
            );
        };
        if found.verdict == Word::None {
            return Response::error(
                ErrorCode::Invalid,
                "a `none` verdict counts at no gate: there is nothing to set aside",
            );
        }
        let by = AgentId::from(actor.id.as_str());
        let by_name = state
            .registry
            .get(&by)
            .map(|r| r.spec.name.clone())
            .unwrap_or_else(|| actor.id.clone());
        let mark = Supersession {
            by,
            by_name,
            with: None,
            role: None,
            reason: SupersedeReason::Person,
            note,
            at: Utc::now(),
        };
        if let Err(reason) = found.supersede(mark.clone()) {
            return Response::error(ErrorCode::Conflict, reason);
        }
        if let Some(error) = state.commit_superseded(&[(&found, &mark, None)]) {
            return error;
        }
        Response::CodeVerdict { verdict: found }
    }
}

/// Whether git in `repository` says `later` is `earlier` or comes after
/// it. Whatever git cannot say — a commit it does not have, no repository
/// there — is no.
fn at_or_after(repository: &std::path::Path, earlier: &str, later: &str) -> bool {
    let argv: Vec<String> = ["git", "merge-base", "--is-ancestor", earlier, later]
        .map(str::to_owned)
        .to_vec();
    agentdocker_host::command::run(repository, &argv, std::time::Duration::from_secs(10))
        .is_ok_and(|output| output.success)
}

/// The event for a verdict set aside by `mark`.
fn superseded_event(verdict: &CodeVerdict, mark: &Supersession) -> EventKind {
    EventKind::CodeVerdictSuperseded {
        project: verdict.project.clone(),
        branch: verdict.branch.clone(),
        verdict: verdict.id.clone(),
        was_by: verdict.by.clone(),
        by: mark.by.clone(),
        reason: mark.reason,
        role: mark.role.clone(),
    }
}

/// The journal's line for a verdict set aside: which verdict, whose, and
/// who set it aside — the successor whose pass did, at which gate and
/// with which commit, or the person.
fn superseded_entry(
    verdict: &CodeVerdict,
    mark: &Supersession,
    pass: Option<&CodeVerdict>,
) -> JournalEntry {
    let short = |commit: &str| commit.chars().take(8).collect::<String>();
    let word = match verdict.verdict {
        Word::Pass => "pass of",
        Word::Changes => "request for changes on",
        Word::None => "reading of",
    };
    let mut summary = format!(
        "lifted {}'s {word} {} at {} (verdict {})",
        verdict.by_name,
        verdict.branch,
        short(&verdict.head),
        verdict.id
    );
    match (mark.reason, pass) {
        (SupersedeReason::ReviewerEnded, Some(pass)) => summary.push_str(&format!(
            " at the {} gate: {} had left, and {} passed {} since (verdict {})",
            mark.role.as_deref().unwrap_or(GATE_ROLE),
            verdict.by_name,
            mark.by_name,
            short(&pass.head),
            pass.id
        )),
        _ if !mark.note.is_empty() => {
            let note: String = mark.note.chars().take(300).collect();
            summary.push_str(": ");
            summary.push_str(&note);
        }
        _ => {}
    }
    JournalEntry {
        project: verdict.project.clone(),
        seq: 0,
        at: mark.at,
        agent: Some(mark.by.clone()),
        agent_name: mark.by_name.clone(),
        branch: Some(verdict.branch.clone()),
        checkout: None,
        worktree: None,
        kind: JournalKind::Review,
        summary,
        summary_source: SummarySource::Explicit,
        resources: Vec::new(),
        paths: Vec::new(),
        paths_total: 0,
        head_before: None,
        head_after: Some(verdict.head.clone()),
        changes: None,
    }
}

/// How a walk up from a verdict's sender ended.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::daemon) enum Author {
    Reviewer,
    Other(&'static str),
}

/// The dedicated Codex server `agentdocker codex` starts beside a
/// conversation's terminal: a Codex app-server listening on loopback, a
/// child of the same launcher as the reviewer's terminal, born after it,
/// where the launcher is AgentDocker's own CLI's `codex` or `codex-native`
/// (this version's, or a retained one's that launched it before an update).
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
    // The launcher is AgentDocker's own CLI, by the file the kernel says it
    // runs; and the terminal speaks to this server, on the
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
/// A worktree hook's report of a commit is held to the same test.
pub(in crate::daemon) fn author(
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
            // By the file, not its name: the companion CLI is the daemon's
            // own file, which macOS may name by either of its names; or a
            // retained version's, which launched a session before an update.
            crate::daemon::person::own_cli,
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
    Ok(crate::daemon::authorship::head_ref(checkout)?
        .and_then(|full| full.strip_prefix("refs/heads/").map(str::to_owned)))
}

/// The journal's line for a verdict: who said what of which branch, at
/// which commit, from which, in which runtime, with what found. The app
/// shows this line where a verdict appears, so it carries the counts and
/// the runtime.
fn journal_entry(verdict: &CodeVerdict) -> JournalEntry {
    let short = |commit: &str| commit.chars().take(8).collect::<String>();
    let word = match verdict.verdict {
        Word::Pass => "passed",
        Word::Changes => "asked for changes on",
        Word::None => "read, with no verdict,",
    };
    let range = match &verdict.from {
        Some(from) => format!("{}..{}", short(from), short(&verdict.head)),
        None => short(&verdict.head),
    };
    let mut summary = format!(
        "{word} {} at {range} as {} ({})",
        verdict.branch,
        verdict.runtime_said(),
        verdict.findings.said()
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
            Response::ReviewGate { gate, .. } => gate,
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
        // A short commit, a bad branch, a word outside the vocabulary, or
        // a pass carrying an important finding is not a verdict.
        assert!(refused(
            daemon
                .handle(verdict(&reviewer, None, "feature/x", "abc1234", "pass"))
                .await,
            ErrorCode::Invalid
        ));
        assert!(refused(
            daemon
                .handle(verdict(&reviewer, None, "a..b", A, "pass"))
                .await,
            ErrorCode::Invalid
        ));
        assert!(refused(
            daemon
                .handle(verdict(&reviewer, None, "feature/x", A, "lgtm"))
                .await,
            ErrorCode::Invalid
        ));
        let mut important = verdict(&reviewer, None, "feature/x", A, "pass");
        if let Request::CodeVerdict { findings, .. } = &mut important {
            findings.important = 1;
        }
        assert!(refused(daemon.handle(important).await, ErrorCode::Invalid));

        let mut changes = verdict(&reviewer, None, "feature/x", A, "changes");
        if let Request::CodeVerdict { findings, .. } = &mut changes {
            *findings = Findings {
                important: 1,
                nit: 2,
                pre_existing: 3,
            };
        }
        let said = daemon.handle(changes).await;
        let Response::CodeVerdict { verdict: given } = said else {
            panic!("{said:?}");
        };
        assert_eq!(given.verdict, Word::Changes);
        assert_eq!(given.findings.said(), "important 1, nit 2, pre-existing 3");
        assert_eq!(given.runtime, "", "registered with no runtime named");
        assert!(matches!(
            ask(&daemon, &author, A).await,
            gate::Gate::Changes { .. }
        ));
        // The older spelling still reads as a pass, and a verdict given
        // without counts has none.
        let older: Request = serde_json::from_value(serde_json::json!({
            "op": "code_verdict", "agent": reviewer, "branch": "feature/x",
            "head": A, "verdict": "approve"
        }))
        .unwrap();
        let Response::CodeVerdict { verdict: given } = daemon.handle(older).await else {
            panic!("the older spelling is read");
        };
        assert_eq!(given.verdict, Word::Pass);
        assert_eq!(given.findings, Findings::default());
        assert!(ask(&daemon, &author, A).await.approved());
        // A commit since, a merge of main included, makes it stale.
        assert!(matches!(
            ask(&daemon, &author, B).await,
            gate::Gate::Stale { verdict } if verdict.head == A
        ));
        // The person's word counts without a role, in the project named.
        let said = daemon
            .handle(verdict(HUMAN, Some(&project), "feature/x", B, "pass"))
            .await;
        let Response::CodeVerdict { verdict: given } = said else {
            panic!("{said:?}");
        };
        assert_eq!(given.runtime, HUMAN_RUNTIME);
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
        assert!(lines.iter().any(|e| {
            e.head_after.as_deref() == Some(B)
                && e.summary
                    .starts_with("passed feature/x at bbbbbbbb as human (important 0,")
        }));
        assert!(
            lines.iter().any(|e| e
                .summary
                .contains("as unknown (important 1, nit 2, pre-existing 3)")),
            "{lines:?}"
        );
    }

    /// The event carries the word, the counts and the runtime, so a
    /// listener needs no second read.
    #[tokio::test]
    async fn the_event_carries_the_counts_and_the_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = open(&tmp);
        let repo = tmp.path().join("repo");
        repository(&repo, "main");
        let reviewer = match daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "reviewer".into(),
                    runtime: "codex".into(),
                    workdir: Some(repo.clone()),
                    ..AgentSpec::default()
                },
                pid: None,
                session: None,
            })
            .await
        {
            Response::Agent { agent } => agent,
            other => panic!("{other:?}"),
        };
        give_role(&daemon, &reviewer.id, "reviewer");
        let mut request = verdict(reviewer.id.as_str(), None, "feature/x", A, "none");
        if let Request::CodeVerdict { findings, .. } = &mut request {
            findings.nit = 2;
        }
        let Response::CodeVerdict { verdict: given } = daemon.handle(request).await else {
            panic!("given");
        };
        assert_eq!(given.runtime, "codex");
        assert_eq!(
            given.line(),
            format!(
                "verdict=none head={A} important=0 nit=2 pre_existing=0 runtime=codex by=reviewer"
            )
        );
        let events = daemon.recent_events(200);
        let carried = events.iter().find_map(|e| match &e.kind {
            EventKind::CodeVerdictGiven {
                verdict,
                findings,
                runtime,
                ..
            } => Some((*verdict, *findings, runtime.clone())),
            _ => None,
        });
        assert_eq!(
            carried,
            Some((
                Word::None,
                Findings {
                    important: 0,
                    nit: 2,
                    pre_existing: 0
                },
                "codex".to_owned()
            ))
        );
    }

    /// The author may not pass its own change: holding the branch's lease,
    /// or a journal line committing to the branch, refuses the word, each
    /// saying why.
    #[tokio::test]
    async fn an_agent_does_not_review_a_branch_it_holds_or_the_journal_says_it_committed_to() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = open(&tmp);
        let repo = tmp.path().join("repo");
        repository(&repo, "main");
        let reviewer = register(&daemon, "reviewer", &repo).await;
        give_role(&daemon, &reviewer.id, "reviewer");
        let project = reviewer.project.as_ref().unwrap().id().clone();
        let claim: Request = serde_json::from_value(serde_json::json!({
            "op": "claim", "agent": reviewer.id.to_string(),
            "resource": "branch:feature/x", "mode": "exclusive", "ttl_secs": 600
        }))
        .unwrap();
        let Response::Lease { lease } = daemon.handle(claim).await else {
            panic!("the lease is taken");
        };
        let refusal = daemon
            .handle(verdict(reviewer.id.as_str(), None, "feature/x", A, "pass"))
            .await;
        assert!(
            matches!(&refusal, Response::Error { code: ErrorCode::Forbidden, message, .. }
                if message.contains("holds") && message.contains(lease.id.as_str())),
            "{refusal:?}"
        );
        // Another branch's lease is no bar.
        let said = daemon
            .handle(verdict(reviewer.id.as_str(), None, "feature/y", A, "pass"))
            .await;
        assert!(matches!(said, Response::CodeVerdict { .. }), "{said:?}");
        let release: Request = serde_json::from_value(serde_json::json!({
            "op": "release", "agent": reviewer.id.to_string(), "lease": lease.id.as_str()
        }))
        .unwrap();
        daemon.handle(release).await;
        let said = daemon
            .handle(verdict(reviewer.id.as_str(), None, "feature/x", A, "none"))
            .await;
        assert!(matches!(said, Response::CodeVerdict { .. }), "{said:?}");

        // The journal attributes a commit on the branch to it.
        {
            let mut state = lock(&daemon.state);
            let record = state.registry.get(&reviewer.id).cloned().unwrap();
            let mut entry = state
                .plain_entry(
                    &record,
                    JournalKind::Commit,
                    "committed aaaaaaaa: wrote it".to_owned(),
                    SummarySource::Explicit,
                )
                .unwrap();
            entry.project = project.clone();
            entry.branch = Some("feature/x".into());
            state.append_journal(entry);
        }
        let refusal = daemon
            .handle(verdict(reviewer.id.as_str(), None, "feature/x", A, "pass"))
            .await;
        assert!(
            matches!(&refusal, Response::Error { code: ErrorCode::Forbidden, message, .. }
                if message.contains("journal")),
            "{refusal:?}"
        );
        let said = daemon
            .handle(verdict(reviewer.id.as_str(), None, "feature/y", A, "pass"))
            .await;
        assert!(matches!(said, Response::CodeVerdict { .. }), "{said:?}");
    }

    /// Make an empty commit in `repo`; its full id.
    fn commit_in(repo: &std::path::Path, message: &str) -> String {
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(["-c", "user.email=t@example.invalid", "-c", "user.name=T"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(repo)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}: {output:?}");
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        };
        git(&["commit", "-q", "--allow-empty", "-m", message]);
        git(&["rev-parse", "HEAD"])
    }

    /// A project's review lines.
    fn journal_request(project: &str) -> Request {
        serde_json::from_value(serde_json::json!({
            "op": "journal", "project": project, "kind": "review", "limit": 50
        }))
        .unwrap()
    }

    fn superseded_events(daemon: &Arc<Daemon>, id: &str) -> Vec<EventKind> {
        daemon
            .recent_events(500)
            .into_iter()
            .map(|e| e.kind)
            .filter(|kind| {
                matches!(kind, EventKind::CodeVerdictSuperseded { verdict, .. } if verdict == id)
            })
            .collect()
    }

    #[tokio::test]
    async fn a_departed_reviewer_s_changes_are_lifted_by_its_successor_s_pass_of_a_later_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = open(&tmp);
        let repo = tmp.path().join("repo");
        repository(&repo, "main");
        // The commit the first reviewer read, and the fix after it.
        let read = commit_in(&repo, "first");
        let fixed = commit_in(&repo, "fix");
        let author = register(&daemon, "author", &repo).await;
        let first = register(&daemon, "reviewer-1", &repo).await;
        let second = register(&daemon, "reviewer-2", &repo).await;
        let project = author.project.as_ref().unwrap().id().to_string();
        let author = author.id.to_string();
        give_role(&daemon, &first.id, "reviewer");
        give_role(&daemon, &second.id, "reviewer");
        let give = |by: &AgentRecord, head: &str, word: &str| {
            let request = verdict(by.id.as_str(), None, "feature/x", head, word);
            let daemon = daemon.clone();
            async move {
                match daemon.handle(request).await {
                    Response::CodeVerdict { verdict } => verdict,
                    other => panic!("{other:?}"),
                }
            }
        };
        let changes = give(&first, &read, "changes").await;
        // A verdict withheld is not a pass.
        give(&second, &fixed, "comment").await;
        let blocked_by_first = |gate: gate::Gate| matches!(gate, gate::Gate::Changes { verdict } if verdict.id == changes.id);
        assert!(
            blocked_by_first(ask(&daemon, &author, &fixed).await),
            "live"
        );
        lock(&daemon.state)
            .registry
            .get_mut(&first.id)
            .unwrap()
            .status = AgentStatus::Exited { code: Some(0) };
        assert!(
            blocked_by_first(ask(&daemon, &author, &fixed).await),
            "a comment"
        );
        // A pass of a commit git cannot place after the one read lifts
        // nothing.
        give(&second, B, "approve").await;
        assert!(
            blocked_by_first(ask(&daemon, &author, B).await),
            "an unknown commit"
        );
        assert!(superseded_events(&daemon, &changes.id).is_empty());

        // The successor's pass of the fix lifts it, and the gate says which
        // verdict and by whom.
        let pass = give(&second, &fixed, "approve").await;
        let Response::ReviewGate { gate, superseded } = daemon
            .handle(Request::ReviewGate {
                agent: Some(author.clone()),
                project: None,
                branch: "feature/x".into(),
                head: fixed.clone(),
                role: None,
            })
            .await
        else {
            panic!("no gate");
        };
        assert!(gate.approved(), "{gate:?}");
        assert_eq!(superseded.len(), 1);
        assert_eq!(superseded[0].id, changes.id);
        let mark = superseded[0].superseded_at(GATE_ROLE).unwrap();
        assert_eq!(mark.by, second.id);
        assert_eq!(mark.with.as_deref(), Some(pass.id.as_str()));
        assert_eq!(mark.role.as_deref(), Some(GATE_ROLE));
        assert_eq!(mark.reason, SupersedeReason::ReviewerEnded);
        // Kept on the verdict, announced once however often the gate is
        // asked, and journaled naming all three.
        let kept = lock(&daemon.state)
            .project_verdicts(&ProjectId::from(project.as_str()))
            .unwrap();
        assert!(
            kept.iter()
                .any(|v| v.id == changes.id && !v.superseded.is_empty())
        );
        assert!(ask(&daemon, &author, &fixed).await.approved());
        let events = superseded_events(&daemon, &changes.id);
        assert_eq!(events.len(), 1, "once");
        assert!(matches!(
            &events[0],
            EventKind::CodeVerdictSuperseded {
                was_by, by, reason: SupersedeReason::ReviewerEnded, role: Some(role), ..
            } if *was_by == first.id && *by == second.id && role == GATE_ROLE
        ));
        let journal: Request = serde_json::from_value(serde_json::json!({
            "op": "journal", "project": project, "kind": "review", "limit": 50
        }))
        .unwrap();
        let Response::Journal { entries, .. } = daemon.handle(journal).await else {
            panic!("no journal");
        };
        let line = entries
            .iter()
            .find(|e| {
                e.summary
                    .starts_with("lifted reviewer-1's request for changes on feature/x")
            })
            .unwrap_or_else(|| panic!("{entries:?}"));
        assert!(line.summary.contains(&changes.id), "{}", line.summary);
        assert!(
            line.summary
                .contains(&format!("reviewer-2 passed {}", &fixed[..8])),
            "{}",
            line.summary
        );
        assert_eq!(line.agent.as_ref(), Some(&second.id));

        // Only the person sets a verdict aside: in the project they name,
        // not a comment, and once.
        let lift = |agent: &str, project: Option<&str>, id: &str| Request::SupersedeVerdict {
            agent: agent.to_owned(),
            project: project.map(str::to_owned),
            verdict: id.to_owned(),
            reason: Some("the reviewer is gone".into()),
        };
        assert!(refused(
            daemon
                .handle(lift(second.id.as_str(), None, &pass.id))
                .await,
            ErrorCode::Forbidden
        ));
        assert!(refused(
            daemon.handle(lift(HUMAN, None, &pass.id)).await,
            ErrorCode::Invalid
        ));
        assert!(refused(
            daemon.handle(lift(HUMAN, Some(&project), "nope")).await,
            ErrorCode::NotFound
        ));
        let comment = kept.iter().find(|v| v.verdict == Word::None).unwrap();
        assert!(refused(
            daemon
                .handle(lift(HUMAN, Some(&project), &comment.id))
                .await,
            ErrorCode::Invalid
        ));
        let Response::CodeVerdict { verdict: set_aside } =
            daemon.handle(lift(HUMAN, Some(&project), &pass.id)).await
        else {
            panic!("not set aside");
        };
        let mark = set_aside.superseded_at("tester").unwrap();
        assert_eq!(
            (mark.reason, mark.role.as_deref()),
            (SupersedeReason::Person, None)
        );
        assert_eq!(mark.note, "the reviewer is gone");
        assert!(refused(
            daemon.handle(lift(HUMAN, Some(&project), &pass.id)).await,
            ErrorCode::Conflict
        ));
        // The fix's pass gone too, nobody has a word at the fix: the
        // successor's earlier pass of the unknown commit stays replaced.
        assert_eq!(ask(&daemon, &author, &fixed).await, gate::Gate::Missing);
        assert!(matches!(
            superseded_events(&daemon, &pass.id).as_slice(),
            [EventKind::CodeVerdictSuperseded {
                reason: SupersedeReason::Person,
                role: None,
                ..
            }]
        ));
        // Journaled with the person's reason, in the commit that kept the
        // mark.
        let Response::Journal { entries, .. } = daemon.handle(journal_request(&project)).await
        else {
            panic!("no journal");
        };
        let line = format!(
            "lifted {}'s pass of feature/x at {} (verdict {}): the reviewer is gone",
            pass.by_name,
            &fixed[..8],
            pass.id
        );
        assert!(entries.iter().any(|e| e.summary == line), "{entries:?}");
        // A pass the person revoked is kept where a daemon that reads no
        // marks, and so would count it again, cannot open it: schema 30 was
        // the last without them, and an older build refuses a newer one.
        const LAST_WITHOUT_MARKS: i64 = 30;
        let recorded = lock(&daemon.state).store.recorded_schema_version().unwrap();
        assert!(recorded > LAST_WITHOUT_MARKS, "recorded {recorded}");
    }

    /// A project where the first reviewer asked for changes at `read` and
    /// has left, and the second, holding the reviewer role, passed `fixed`,
    /// which comes after it; nobody has asked the gate since.
    struct Departed {
        _tmp: TempDir,
        daemon: Arc<Daemon>,
        project: String,
        author: String,
        fixed: String,
        changes: CodeVerdict,
    }

    async fn departed() -> Departed {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = open(&tmp);
        let repo = tmp.path().join("repo");
        repository(&repo, "main");
        let read = commit_in(&repo, "first");
        let fixed = commit_in(&repo, "fix");
        let author = register(&daemon, "author", &repo).await;
        let first = register(&daemon, "reviewer-1", &repo).await;
        let second = register(&daemon, "reviewer-2", &repo).await;
        give_role(&daemon, &first.id, "reviewer");
        give_role(&daemon, &second.id, "reviewer");
        let Response::CodeVerdict { verdict: changes } = daemon
            .handle(verdict(
                first.id.as_str(),
                None,
                "feature/x",
                &read,
                "changes",
            ))
            .await
        else {
            panic!("no verdict");
        };
        lock(&daemon.state)
            .registry
            .get_mut(&first.id)
            .unwrap()
            .status = AgentStatus::Exited { code: Some(0) };
        let said = daemon
            .handle(verdict(
                second.id.as_str(),
                None,
                "feature/x",
                &fixed,
                "pass",
            ))
            .await;
        assert!(matches!(said, Response::CodeVerdict { .. }), "{said:?}");
        Departed {
            project: author.project.as_ref().unwrap().id().to_string(),
            author: author.id.to_string(),
            _tmp: tmp,
            daemon,
            fixed,
            changes,
        }
    }

    #[tokio::test]
    async fn a_lift_whose_journal_line_cannot_be_written_is_not_kept_at_all() {
        for by_the_person in [false, true] {
            let departed = departed().await;
            let daemon = &departed.daemon;
            let conn = crate::sqlite_fixture::open(daemon.home.join("state.db")).unwrap();
            conn.execute_batch(
                "CREATE TRIGGER fail BEFORE INSERT ON journal BEGIN SELECT RAISE(ABORT, 'journal fault'); END;",
            )
            .unwrap();
            let request = if by_the_person {
                Request::SupersedeVerdict {
                    agent: HUMAN.into(),
                    project: Some(departed.project.clone()),
                    verdict: departed.changes.id.clone(),
                    reason: None,
                }
            } else {
                Request::ReviewGate {
                    agent: Some(departed.author.clone()),
                    project: None,
                    branch: "feature/x".into(),
                    head: departed.fixed.clone(),
                    role: None,
                }
            };
            let answer = daemon.handle(request).await;
            assert!(
                refused(answer.clone(), ErrorCode::StorageUnavailable),
                "{by_the_person}: {answer:?}"
            );
            let state = lock(&daemon.state);
            assert!(state.storage_error.is_some(), "the fault fired");
            let kept: Vec<CodeVerdict> = state
                .store
                .documents_where(VERDICT, "project", &departed.project, 100)
                .unwrap();
            assert!(
                kept.iter().all(|v| v.superseded.is_empty()),
                "no mark without its line"
            );
            assert!(
                !state
                    .store
                    .recent_events(500)
                    .unwrap()
                    .iter()
                    .any(|e| matches!(e.kind, EventKind::CodeVerdictSuperseded { .. })),
                "no event without its line"
            );
            let lines: i64 = conn
                .query_row(
                    "SELECT count(*) FROM journal WHERE summary LIKE 'lifted %'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(lines, 0);
        }
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
                    "pass"
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
                        .handle(verdict(reviewer.id.as_str(), None, own, A, "pass"))
                        .await,
                    ErrorCode::Forbidden
                ),
                "{own:?}"
            );
        }
        // Another branch is its to review.
        let said = daemon
            .handle(verdict(reviewer.id.as_str(), None, "feature/x", A, "pass"))
            .await;
        assert!(matches!(said, Response::CodeVerdict { .. }), "{said:?}");
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
        let mut request = verdict("reviewer", None, "feature/x", A, "pass");
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
        let mut person = verdict(HUMAN, None, "feature/x", A, "pass");
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
        let mut unknown = verdict("unused-name", None, "feature/x", A, "pass");
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
                .handle(verdict(HUMAN, None, "feature/x", A, "pass"))
                .await,
            ErrorCode::Invalid
        ));
        // A reviewer whose checkout git cannot read gives no word.
        let reviewer = register(&daemon, "reviewer", &plain).await;
        give_role(&daemon, &reviewer.id, "reviewer");
        assert!(refused(
            daemon
                .handle(verdict(reviewer.id.as_str(), None, "feature/x", A, "pass"))
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
        let request =
            serde_json::to_string(&verdict(reviewer.id.as_str(), None, "feature/x", A, "pass"))
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
                serde_json::to_string(&verdict(outer.id.as_str(), None, "feature/y", A, "pass"))
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
