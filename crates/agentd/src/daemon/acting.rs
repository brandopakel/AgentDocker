//! Who may act as an agent. The host socket trusts its owning OS user, and
//! every agent's tools run as that same user, so a request's word that agent
//! X sent it is a claim, as the person's word is ([`super::person`]). A
//! request that acts as an agent — its `from`, the `agent` that claims,
//! moves a card or reads its own queue — is let through only when the
//! process on the other end of the socket runs inside that agent's own
//! session, or is the person's. Inside the session: one of the agent's own
//! processes, each by its pid *and* its birth, stands among the sender's
//! parents (the sender itself included) — its recorded process, its session
//! owner, its input controller, or the companion launcher of a Codex
//! terminal, which starts the terminal and the Codex server its tools run
//! under side by side — or, for a Codex agent, the sender runs under Codex's
//! shared app-server, whose conversations its process tree cannot tell
//! apart. The person's: outside every agent's session, as the person check
//! traces it (the app, their own terminal). The kernel says which process
//! sent the request; the request cannot.
//!
//! So an agent's hooks, its MCP server, a shell its tools opened and every
//! session those started act as it, and the person may act as any agent;
//! from inside another agent's session, or from a process whose parents
//! cannot be read, the request is refused `identity_mismatch`. The person
//! (`user`, or their record) is the person check's; the daemon's own process
//! is trusted; and a harness's own daemon started with
//! `AGENTDOCKER_AGENT_CHECK=off` skips the check.

use super::*;
use agentdocker_core::HUMAN;
use agentdocker_host::procinfo::{self, Unseen};
use person::Peer;

/// How far up a sender's parents are walked.
const ANCESTRY: usize = 64;

/// One process, by its pid and its birth.
type Process = (u32, DateTime<Utc>);

/// The field of `request` that names the agent it acts as: the sender of a
/// message, card or memory, the holder of a lease, the reader of a queue,
/// the adapter's own agent. `None` for a request that acts as nobody, that
/// names an agent only as what it acts on (`stop`, `rename`, `role_assign`'s
/// `agent`, `worktree_remove`, a launch), or that only reads. A verdict's
/// reviewer has a stricter check of its own (`code_verdict`), and so do the
/// requests an input binding's token speaks for.
pub(crate) fn acting_agent(request: &mut Request) -> Option<&mut String> {
    use Request as R;
    match request {
        R::RecordEmission { agent, .. }
        | R::WorktreeCreate { agent, .. }
        | R::Commit { agent, .. }
        | R::Integrate { agent, .. }
        | R::Checkpoint { agent, .. }
        | R::Resume { agent, .. }
        | R::Handoff { agent, .. }
        | R::Validate { agent, .. }
        | R::Observe { agent, .. }
        | R::Deregister { agent }
        | R::Heartbeat { agent }
        | R::Report { agent, .. }
        | R::ReportActivity { agent, .. }
        | R::ReportAdapter { agent, .. }
        | R::ReportProvider { agent, .. }
        | R::DeliveryQueue { agent }
        | R::Inbox { agent, .. }
        | R::AckInbox { agent, .. }
        | R::CancelQuestion { agent, .. }
        | R::TaskPull { agent, .. }
        | R::TaskMove { agent, .. }
        | R::TaskUpdate { agent, .. }
        | R::TaskArchive { agent, .. }
        | R::TaskUnarchive { agent, .. }
        | R::ContestOpen { agent, .. }
        | R::ContestEnter { agent, .. }
        | R::ContestSubmit { agent, .. }
        | R::ContestClose { agent, .. }
        | R::Claim { agent, .. }
        | R::Renew { agent, .. }
        | R::Release { agent, .. }
        | R::ReleaseAll { agent, .. }
        | R::JournalAdd { agent, .. }
        | R::ChannelOpen { agent, .. }
        | R::ChannelInvite { agent, .. }
        | R::ChannelClose { agent, .. }
        | R::ReviewRequest { agent, .. }
        | R::Review { agent, .. } => Some(agent),
        R::MergeBranch { from, .. }
        | R::Send { from, .. }
        | R::AnswerSecretReview { from, .. }
        | R::CancelSecretReview { from, .. }
        | R::Ask { from, .. }
        | R::PostQuestion { from, .. }
        | R::Pause { from, .. }
        | R::ResumeProject { from, .. }
        | R::TaskCreate { from, .. }
        | R::TaskCreateMany { from, .. }
        | R::TaskArchiveLane { from, .. }
        | R::TaskComment { from, .. }
        | R::TaskCommentEdit { from, .. }
        | R::TaskCommentDelete { from, .. }
        | R::BoardCreate { from, .. }
        | R::BoardUpdate { from, .. }
        | R::RuleCreate { from, .. }
        | R::RuleUpdate { from, .. }
        | R::RuleDelete { from, .. }
        | R::TaskStart { from, .. }
        | R::TagSave { from, .. }
        | R::TagDelete { from, .. }
        | R::RoleSave { from, .. }
        | R::RoleAssign { from, .. }
        | R::RoleDelete { from, .. }
        | R::MemoryAdd { from, .. }
        | R::MemoryForget { from, .. }
        | R::PlanSave { from, .. }
        | R::FilePut { from, .. }
        | R::FileLink { from, .. }
        | R::FileDelete { from, .. } => Some(from),
        R::Subscribe { agent, .. } => agent.as_mut(),
        R::Answer { from, .. } => from.as_mut(),
        R::MarkRead { reader, .. } => reader.as_mut(),
        // A digest that moves a reader's cursor acts for that reader.
        R::Journal {
            digest: Some(digest),
            ..
        } if digest.advance => Some(&mut digest.reader),
        // The bound controller's token speaks for these, and a binding is
        // made only for the live provider generation it names.
        R::BindInput { .. }
        | R::UpgradeController { .. }
        | R::ReportInput { .. }
        | R::ProviderInbox { .. }
        | R::UnbindInput { .. }
        | R::ResumeInput { .. }
        // The exact live owner process these name is checked by them.
        | R::OpenSecretReview { .. }
        | R::PollSecretReview { .. }
        | R::CodeVerdict { .. }
        // What is acted on, not who acts.
        | R::WorktreeRemove { .. }
        | R::GrantAccess { .. }
        | R::Import { .. }
        | R::Run { .. }
        | R::ResumeSession { .. }
        | R::RunContainer { .. }
        | R::RestartContainer { .. }
        | R::Register { .. }
        | R::SessionAttach { .. }
        | R::Adopt { .. }
        | R::Stop { .. }
        | R::Remove { .. }
        | R::Role { .. }
        | R::Rename { .. }
        | R::RetryController { .. }
        | R::ResumeProvider { .. }
        | R::Attach { .. }
        | R::Me { .. }
        | R::CheckpointPrune { .. }
        // Nobody, or only reads.
        | R::Ping
        | R::Usage { .. }
        | R::BuildImage { .. }
        | R::Images
        | R::WorktreeDiff { .. }
        | R::ReviewBranch { .. }
        | R::Authenticate { .. }
        | R::RevokeAccess { .. }
        | R::Checkpoints { .. }
        | R::Handoffs { .. }
        | R::Validations { .. }
        | R::Stale { .. }
        | R::Reads { .. }
        | R::DaemonProcess
        | R::Discover
        | R::Runtimes
        | R::List { .. }
        | R::Inspect { .. }
        | R::Changes { .. }
        | R::Shutdown
        | R::Reload
        | R::PeekInput { .. }
        | R::SecretReviews { .. }
        | R::Questions { .. }
        | R::Activity { .. }
        | R::Waiting
        | R::Pauses
        | R::TaskGet { .. }
        | R::Boards { .. }
        | R::Rules { .. }
        | R::Tags { .. }
        | R::RoleCatalog { .. }
        | R::TaskSearch { .. }
        | R::Plans { .. }
        | R::MemorySearch { .. }
        | R::MemoryProfile { .. }
        | R::PlanGet { .. }
        | R::Files { .. }
        | R::FileGet { .. }
        | R::Tasks { .. }
        | R::Contests { .. }
        | R::Journal { .. }
        | R::Overlap { .. }
        | R::Conversations { .. }
        | R::Delivery { .. }
        | R::History { .. }
        | R::Thread { .. }
        | R::SearchMessages { .. }
        | R::Channels { .. }
        | R::ChannelPrune { .. }
        | R::ReviewGate { .. }
        | R::AttachInput { .. }
        | R::AttachResize { .. }
        | R::JournalPrune { .. }
        | R::Vacuum { .. }
        | R::Leases { .. }
        | R::WebhookDeliveries { .. }
        | R::Events { .. }
        | R::ResumeEvents { .. }
        | R::Logs { .. } => None,
    }
}

/// How a walk up a sender's parents ended.
#[derive(Clone, Debug, PartialEq, Eq)]
enum End {
    /// At one of the agent's own processes.
    Found,
    /// At the first process, at a process that is its own parent, or at a
    /// boundary past which nothing is this user's doing: a parent that has
    /// ended, another user's, or a "parent" born after its child (its pid
    /// was reused).
    Top,
    /// At this user's process that still runs and could not be read, or
    /// deeper than can be traced: nothing above it is known.
    Unknown,
}

/// The sender and its parents, nearest first, each by its pid and birth
/// (`step` reads a process's parent and birth), walked until one is
/// `inside` or the walk can go no further, and how it ended. The sender is
/// the first only while it is still the process that connected (`born`).
fn lineage(
    sender: u32,
    born: DateTime<Utc>,
    step: impl Fn(u32) -> Option<(u32, DateTime<Utc>)>,
    unseen: impl Fn(u32) -> Unseen,
    inside: impl Fn(Process) -> bool,
) -> (Vec<Process>, End) {
    let mut chain = Vec::new();
    let mut pid = sender;
    let mut child: Option<DateTime<Utc>> = None;
    for _ in 0..ANCESTRY {
        if pid <= 1 {
            return (chain, End::Top);
        }
        let Some((parent, birth)) = step(pid) else {
            if child.is_none() {
                return (chain, End::Unknown);
            }
            return match unseen(pid) {
                Unseen::Ended | Unseen::OtherUser => (chain, End::Top),
                Unseen::Unreadable => (chain, End::Unknown),
            };
        };
        match child {
            None if birth != born => return (chain, End::Unknown),
            Some(child) if birth > child => return (chain, End::Top),
            _ => {}
        }
        chain.push((pid, birth));
        if inside((pid, birth)) {
            return (chain, End::Found);
        }
        if parent == pid {
            return (chain, End::Top);
        }
        child = Some(birth);
        pid = parent;
    }
    (chain, End::Unknown)
}

/// The companion launcher a Codex terminal (`terminal`, by pid and birth)
/// was started by: its parent, born before it, running this daemon's own
/// CLI (`companion` says of an executable whether it is that file) as
/// `agentdocker codex` or `codex-native`. That launcher starts the terminal
/// and, beside it, the Codex server its tools run under and its input
/// receiver; nothing else runs under it.
fn codex_launcher(
    terminal: Process,
    step: impl Fn(u32) -> Option<(u32, DateTime<Utc>)>,
    arguments: impl Fn(u32) -> Option<Vec<String>>,
    executable: impl Fn(u32) -> Option<std::path::PathBuf>,
    companion: impl Fn(&std::path::Path) -> bool,
) -> Option<Process> {
    let (pid, born) = terminal;
    let (launcher, _) = step(pid).filter(|(_, birth)| *birth == born)?;
    if launcher <= 1 {
        return None;
    }
    let (_, launched) = step(launcher).filter(|(_, launched)| *launched <= born)?;
    let argv = arguments(launcher)?;
    (argv
        .iter()
        .skip(1)
        .any(|argument| argument == "codex" || argument == "codex-native")
        && executable(launcher).is_some_and(|exe| companion(&exe)))
    .then_some((launcher, launched))
}

/// Whether Codex's shared app-server (`--managed-daemon`) stands among
/// `chain`, each read by its own arguments while it is still the process
/// walked (`born` reads a birth again).
fn under_shared_codex(
    chain: &[Process],
    arguments: impl Fn(u32) -> Option<Vec<String>>,
    born: impl Fn(u32) -> Option<DateTime<Utc>>,
) -> bool {
    chain.iter().any(|(pid, birth)| {
        arguments(*pid).is_some_and(|argv| procinfo::detached_codex_app_server(&argv))
            && born(*pid) == Some(*birth)
    })
}

/// Whether the sender of a request acting as an agent whose own processes
/// are `anchors` runs inside that agent's session: one of them stands
/// among its parents; or, for a Codex agent (`codex`, with its terminal
/// when it has one), the terminal's companion launcher does, or the sender
/// runs under Codex's shared server.
fn inside_session(
    sender: u32,
    born: DateTime<Utc>,
    anchors: &[Process],
    codex: Option<Option<Process>>,
) -> bool {
    let (chain, end) = lineage(
        sender,
        born,
        procinfo::parent_and_birth,
        procinfo::unseen,
        |process| anchors.contains(&process),
    );
    if end == End::Found {
        return true;
    }
    let Some(terminal) = codex else {
        return false;
    };
    terminal
        .and_then(|terminal| {
            codex_launcher(
                terminal,
                procinfo::parent_and_birth,
                procinfo::arguments,
                |pid| procinfo::executable_path_of(pid).ok(),
                |exe| {
                    person::companion_cli()
                        .is_some_and(|cli| agentdocker_host::program::same_file(&cli, exe))
                },
            )
        })
        .is_some_and(|launcher| chain.contains(&launcher))
        || under_shared_codex(&chain, procinfo::arguments, procinfo::start_time)
}

/// What the request acts as, read under the lock.
struct Acting {
    id: AgentId,
    name: String,
    /// Its own processes: its process, its session owner, its input
    /// controller (bound, or launched and not yet bound) and the provider
    /// process the binding names.
    anchors: Vec<Process>,
    /// For a Codex agent, its own process when it has one: its launcher
    /// and Codex's shared server are looked for too.
    codex: Option<Option<Process>>,
}

fn refusal(acting: &Acting, why: &str) -> Response {
    Response::Error {
        code: ErrorCode::IdentityMismatch,
        message: format!(
            "this acts as agent {}, and {why}; an agent acts only as itself, from inside its own session, and the person from outside every agent's",
            acting.name
        ),
        details: Some(json!({"agent": acting.id, "name": acting.name})),
    }
}

impl Daemon {
    /// A refusal when `request` acts as an agent and `peer`, the process on
    /// the socket, runs neither inside that agent's session nor outside
    /// every agent's (the person's). The acting agent's name is replaced
    /// by its id, so a rename while the sender is traced cannot hand its
    /// word to another record. `None` lets it through.
    pub(crate) async fn agent_refusal(
        self: &Arc<Self>,
        peer: Option<Peer>,
        request: &mut Request,
    ) -> Option<Response> {
        if !self.agent_check || peer.is_some_and(|(pid, _)| pid == std::process::id()) {
            return None;
        }
        let field = acting_agent(request)?;
        let acting = {
            let state = lock(&self.state);
            if field == HUMAN {
                return None;
            }
            // A name that resolves to nobody is the handler's to answer.
            let id = state.registry.resolve(field).ok()?;
            let record = state.registry.get(&id)?;
            if humans::is_human(record) {
                return None;
            }
            let own = record.pid.zip(record.process_started_at);
            let mut anchors: Vec<Process> = own.into_iter().collect();
            if let Some(owner) = &record.owner {
                anchors.push((owner.pid, owner.started_at));
            }
            if let Some(binding) = &record.input_binding {
                let launched = binding.restart.launched.as_ref();
                for process in [
                    Some(&binding.controller),
                    Some(&binding.provider.process),
                    launched,
                ]
                .into_iter()
                .flatten()
                {
                    anchors.push((process.pid, process.started_at));
                }
            }
            let acting = Acting {
                name: record.spec.name.clone(),
                codex: (record.spec.runtime == "codex").then_some(own),
                anchors,
                id,
            };
            *field = acting.id.to_string();
            acting
        };
        if let Some((sender, Some(born))) = peer {
            let (anchors, codex) = (acting.anchors.clone(), acting.codex);
            let inside =
                tokio::task::spawn_blocking(move || inside_session(sender, born, &anchors, codex))
                    .await
                    .unwrap_or(false);
            if inside {
                return None;
            }
        }
        // Not the agent's session: the person may act as any agent, and
        // nobody else.
        let why = match self.trace(peer, request).await {
            person::Traced::Person => return None,
            person::Traced::Inside(reason) => {
                format!("it was sent from inside {reason}, not from that agent's session")
            }
            person::Traced::Unknown(reason) => format!(
                "it was not sent from inside that agent's session, and {reason} cannot be the person's"
            ),
        };
        let pid = peer.map_or(0, |(pid, _)| pid);
        warn!(peer = pid, agent = %acting.id, %why, "refused a request acting as an agent");
        Some(refusal(&acting, &why))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::AgentSpec;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + seconds, 0).unwrap()
    }

    fn request(json: serde_json::Value) -> Request {
        serde_json::from_value(json).unwrap()
    }

    /// The field that names who acts, and only that: a target, a launch, a
    /// read or a request with a check of its own names nobody here.
    #[test]
    fn a_request_names_the_agent_it_acts_as() {
        let acting = |json: serde_json::Value| acting_agent(&mut request(json)).cloned();
        let x = Some("x".to_owned());
        for json in [
            serde_json::json!({"op": "send", "from": "x", "to": "y", "kind": "chat", "payload": "hi"}),
            serde_json::json!({"op": "claim", "agent": "x", "resource": "path:/r"}),
            serde_json::json!({"op": "release", "agent": "x", "lease": "l1"}),
            serde_json::json!({"op": "task_move", "agent": "x", "task": "T-1", "to": "done"}),
            serde_json::json!({"op": "task_comment", "from": "x", "task": "T-1", "body": "b"}),
            serde_json::json!({"op": "role_assign", "from": "x", "agent": "y", "add": ["reviewer"]}),
            serde_json::json!({"op": "merge_branch", "from": "x", "agent": "y", "head": "abc"}),
            serde_json::json!({"op": "report_activity", "agent": "x", "observation": {"activity": "working", "observed_at": "2026-01-01T00:00:00Z"}}),
            serde_json::json!({"op": "heartbeat", "agent": "x"}),
            serde_json::json!({"op": "delivery_queue", "agent": "x"}),
            serde_json::json!({"op": "inbox", "agent": "x", "drain": true}),
            serde_json::json!({"op": "subscribe", "agent": "x"}),
            serde_json::json!({"op": "answer", "from": "x", "message": "m1", "text": "Allow"}),
            serde_json::json!({"op": "mark_read", "conversation": "c1", "through": 4, "reader": "x"}),
            serde_json::json!({"op": "journal", "project": "p", "digest": {"reader": "x", "max_entries": 5, "max_chars": 500, "advance": true}}),
            serde_json::json!({"op": "worktree_create", "agent": "x", "path": "/w", "branch": "b", "from": "main"}),
        ] {
            assert_eq!(acting(json.clone()), x, "{json}");
        }
        for json in [
            serde_json::json!({"op": "stop", "agent": "x"}),
            serde_json::json!({"op": "rename", "agent": "x", "name": "n"}),
            serde_json::json!({"op": "worktree_remove", "agent": "x"}),
            serde_json::json!({"op": "inspect", "agent": "x"}),
            serde_json::json!({"op": "peek_input", "agent": "x"}),
            serde_json::json!({"op": "subscribe", "topics": ["t"]}),
            serde_json::json!({"op": "answer", "message": "m1", "text": "Allow"}),
            serde_json::json!({"op": "mark_read", "conversation": "c1", "through": 4}),
            serde_json::json!({"op": "journal", "project": "p", "digest": {"reader": "x", "max_entries": 5, "max_chars": 500}}),
            serde_json::json!({"op": "code_verdict", "agent": "x", "branch": "b", "head": "h", "verdict": "approve"}),
            serde_json::json!({"op": "unbind_input", "agent": "x", "force": true}),
            serde_json::json!({"op": "attach", "agent": "x"}),
        ] {
            assert_eq!(acting(json.clone()), None, "{json}");
        }
    }

    /// The walk goes up by pid and birth and stops at one of the agent's own
    /// processes: a "parent" born after its child had its pid reused, and a
    /// parent that has ended or is another user's is a boundary; this
    /// user's unreadable process, a sender that is no longer the one that
    /// connected, and a chain too deep say nothing more.
    #[test]
    fn a_sender_s_parents_are_walked_by_pid_and_birth_to_the_agent_or_a_boundary() {
        let parents: HashMap<u32, (u32, DateTime<Utc>)> = HashMap::from([
            (40, (30, at(40))),
            (30, (20, at(30))),
            (20, (1, at(20))),
            // 51's "parent" 50 is younger than it: pid 50 was reused.
            (51, (50, at(51))),
            (50, (20, at(99))),
            // 61's parent has ended, 71's is another user's, 81's is this
            // user's and unreadable.
            (61, (60, at(61))),
            (71, (70, at(71))),
            (81, (80, at(81))),
            // Its own parent.
            (90, (90, at(90))),
        ]);
        let step = |pid: u32| parents.get(&pid).copied();
        let unseen = |pid: u32| match pid {
            70 => Unseen::OtherUser,
            80 => Unseen::Unreadable,
            _ => Unseen::Ended,
        };
        let walk = |pid: u32, born: DateTime<Utc>, anchors: &[Process]| {
            lineage(pid, born, step, unseen, |process| {
                anchors.contains(&process)
            })
        };
        assert_eq!(
            walk(40, at(40), &[]),
            (vec![(40, at(40)), (30, at(30)), (20, at(20))], End::Top)
        );
        assert_eq!(
            walk(40, at(40), &[(30, at(30))]),
            (vec![(40, at(40)), (30, at(30))], End::Found),
            "the agent's process among its parents"
        );
        assert_eq!(
            walk(40, at(40), &[(40, at(40))]).1,
            End::Found,
            "the agent's own process"
        );
        assert_eq!(
            walk(40, at(40), &[(30, at(31))]).1,
            End::Top,
            "a reused pid is not the agent's"
        );
        assert_eq!(
            walk(51, at(51), &[(50, at(99))]),
            (vec![(51, at(51))], End::Top),
            "a younger \"parent\" is no parent"
        );
        assert_eq!(walk(61, at(61), &[]).1, End::Top, "an ended parent");
        assert_eq!(walk(71, at(71), &[]).1, End::Top, "another user's parent");
        assert_eq!(
            walk(81, at(81), &[]),
            (vec![(81, at(81))], End::Unknown),
            "this user's unreadable parent"
        );
        assert_eq!(walk(90, at(90), &[]), (vec![(90, at(90))], End::Top));
        assert_eq!(
            walk(40, at(1), &[(40, at(40))]),
            (Vec::new(), End::Unknown),
            "another process has its pid now"
        );
        assert_eq!(walk(41, at(41), &[]), (Vec::new(), End::Unknown), "ended");
        let deep = |pid: u32| Some((pid + 1, at(-i64::from(pid))));
        assert_eq!(lineage(2, at(-2), deep, unseen, |_| false).1, End::Unknown);
    }

    /// Codex's shared app-server counts only by its own arguments and only
    /// while it is still the process walked.
    #[test]
    fn codex_s_shared_server_is_known_by_its_arguments_and_birth() {
        let chain = [(40, at(40)), (30, at(30))];
        let shared = |pid: u32| {
            Some(match pid {
                30 => vec![
                    "codex".to_owned(),
                    "app-server".to_owned(),
                    "--managed-daemon".to_owned(),
                ],
                _ => vec!["agentdocker".to_owned(), "mcp".to_owned()],
            })
        };
        let born = |pid: u32| Some(at(i64::from(pid)));
        assert!(under_shared_codex(&chain, shared, born));
        assert!(
            !under_shared_codex(&chain, shared, |_| Some(at(1))),
            "reused"
        );
        assert!(!under_shared_codex(
            &chain,
            |_| Some(vec!["codex".to_owned(), "app-server".to_owned()]),
            born
        ));
    }

    /// A Codex terminal's launcher counts only as the companion CLI's
    /// `codex` or `codex-native`, the terminal's own parent, born before
    /// it.
    #[test]
    fn a_codex_terminal_s_launcher_is_the_companion_s_codex_command() {
        let terminal = (31, at(31));
        let parents = HashMap::from([(31, (30, at(31))), (30, (20, at(30))), (29, (20, at(40)))]);
        let step = |pid: u32| parents.get(&pid).copied();
        let companion = std::path::PathBuf::from("/opt/agentdocker");
        let is_companion = |exe: &std::path::Path| exe == companion;
        let found = |argv: &'static [&'static str], exe: &'static str| {
            codex_launcher(
                terminal,
                step,
                |_| Some(argv.iter().map(|a| (*a).to_owned()).collect()),
                |_| Some(std::path::PathBuf::from(exe)),
                is_companion,
            )
        };
        assert_eq!(
            found(&["agentdocker", "codex"], "/opt/agentdocker"),
            Some((30, at(30)))
        );
        assert_eq!(
            found(&["agentdocker", "codex-native", "--"], "/opt/agentdocker"),
            Some((30, at(30)))
        );
        assert_eq!(found(&["agentdocker", "mcp"], "/opt/agentdocker"), None);
        assert_eq!(found(&["agentdocker", "codex"], "/tmp/agentdocker"), None);
        assert_eq!(
            codex_launcher(
                (31, at(32)),
                step,
                |_| Some(vec!["agentdocker".into(), "codex".into()]),
                |_| Some(companion.clone()),
                is_companion,
            ),
            None,
            "another terminal now"
        );
        let reused = HashMap::from([(31, (29, at(31))), (29, (20, at(40)))]);
        assert_eq!(
            codex_launcher(
                terminal,
                |pid| reused.get(&pid).copied(),
                |_| Some(vec!["agentdocker".into(), "codex".into()]),
                |_| Some(companion.clone()),
                is_companion,
            ),
            None,
            "a launcher younger than its terminal"
        );
    }

    /// Over the real socket: a request acting as agent X is answered when
    /// its sender runs under X's own process or outside every agent's
    /// session (the person's, as the app's process is), and refused
    /// `identity_mismatch`, with nothing done, from another agent's process
    /// or from inside a provider session that is not X's; an agent with no
    /// process of its own is the person's to act as; the daemon's own
    /// process is trusted, and a sender that cannot be identified is not.
    /// With the check off, nothing is refused.
    #[cfg(unix)]
    #[tokio::test]
    async fn only_x_s_own_session_or_the_person_acts_as_x() {
        use std::io::{BufRead as _, Write as _};
        let tmp = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::open(tmp.path().into(), tmp.path().join("host.sock")).unwrap();
        // The person's own word is the person check's, which this test,
        // perhaps run inside an agent's session, leaves aside.
        daemon.person_check = false;
        let daemon = Arc::new(daemon);
        let listener = crate::server::bind(&daemon).await.unwrap();
        let serving = tokio::spawn(crate::server::serve(daemon.clone(), listener));
        let client = "import os,socket,sys,time\nparent=os.getppid()\nif sys.argv[3]=='orphan':\n    print('ready',flush=True)\n    while os.getppid()==parent:\n        time.sleep(0.01)\ns=socket.socket(socket.AF_UNIX)\ns.connect(sys.argv[1])\ns.sendall(sys.argv[2].encode()+b'\\n')\nprint(s.makefile().readline(),end='',flush=True)\n";
        // A shell (run as `program`) that, once told a request, starts a
        // child that sends it and prints the answer: the shell stays its
        // parent.
        let shell = |program: &std::path::Path, socket: &std::path::Path| {
            std::process::Command::new(program)
                .arg("-c")
                .arg("read request; python3 -c \"$0\" \"$1\" \"$request\" child; exit $?")
                .arg(client)
                .arg(socket)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        };
        let sh = std::path::Path::new("sh");
        let register = |name: &str, pid: Option<u32>| {
            let daemon = daemon.clone();
            let name = name.to_owned();
            let workdir = tmp.path().to_owned();
            async move {
                match daemon
                    .handle(Request::Register {
                        spec: AgentSpec {
                            name,
                            workdir: Some(workdir),
                            ..AgentSpec::default()
                        },
                        pid,
                        session: None,
                    })
                    .await
                {
                    Response::Agent { agent } => agent,
                    other => panic!("{other:?}"),
                }
            }
        };
        let send = |mut child: std::process::Child, request: String| async move {
            writeln!(child.stdin.take().unwrap(), "{request}").unwrap();
            let output = tokio::task::spawn_blocking(move || child.wait_with_output())
                .await
                .unwrap()
                .unwrap();
            serde_json::from_slice::<Response>(&output.stdout).unwrap()
        };
        // A process outside every agent's session: its parent exits before
        // it speaks, so its parent is the first process, as the app's is.
        let orphan = |socket: &std::path::Path, request: String| {
            let mut parent = std::process::Command::new("sh")
                .arg("-c")
                .arg("python3 -c \"$0\" \"$1\" \"$2\" orphan & read line; exit 0")
                .arg(client)
                .arg(socket)
                .arg(request)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            async move {
                let mut out = std::io::BufReader::new(parent.stdout.take().unwrap());
                let mut stdin = parent.stdin.take().unwrap();
                tokio::task::spawn_blocking(move || {
                    let mut line = String::new();
                    out.read_line(&mut line).unwrap();
                    assert_eq!(line.trim(), "ready");
                    writeln!(stdin, "exit").unwrap();
                    parent.wait().unwrap();
                    line.clear();
                    out.read_line(&mut line).unwrap();
                    serde_json::from_str::<Response>(&line).unwrap()
                })
                .await
                .unwrap()
            }
        };
        let claim = |agent: &AgentId, resource: &str| {
            serde_json::json!({
                "op": "claim",
                "agent": agent,
                "resource": format!("path:{}/{resource}", tmp.path().display()),
                "ttl_secs": 60,
            })
            .to_string()
        };
        let refused = |reply: &Response| {
            matches!(
                reply,
                Response::Error {
                    code: ErrorCode::IdentityMismatch,
                    ..
                }
            )
        };
        let held = |reply: &Response| matches!(reply, Response::Lease { .. });

        let x_shell = shell(sh, &daemon.socket);
        let x = register("agent-x", Some(x_shell.id())).await;
        let y_shell = shell(sh, &daemon.socket);
        register("agent-y", Some(y_shell.id())).await;

        // X's own child acts as X.
        let reply = send(x_shell, claim(&x.id, "mine")).await;
        assert!(held(&reply), "{reply:?}");
        // Y's child acting as X is refused, and takes nothing.
        let reply = send(
            y_shell,
            serde_json::json!({"op": "send", "from": "agent-x", "to": "agent-y", "kind": "chat", "payload": "hi"})
                .to_string(),
        )
        .await;
        assert!(refused(&reply), "{reply:?}");
        assert!(
            matches!(&reply, Response::Error { message, details: Some(details), .. }
                if message.contains("agent-x") && message.contains("an agent's process")
                    && details["agent"] == x.id.as_str()),
            "{reply:?}"
        );
        // Nor may a process inside a provider session nobody registered.
        let provider = tmp.path().join("bin").join("claude");
        std::fs::create_dir_all(provider.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink("/bin/sh", &provider).unwrap();
        let reply = send(shell(&provider, &daemon.socket), claim(&x.id, "taken")).await;
        assert!(refused(&reply), "{reply:?}");
        assert_eq!(lock(&daemon.state).leases.len(), 1, "only X's claim");

        // The person, outside every agent's session, may act as X.
        let reply = orphan(&daemon.socket, claim(&x.id, "persons")).await;
        assert!(held(&reply), "{reply:?}");

        // An agent with no process of its own, such as a connector's, is
        // the person's to act as, and nobody else's.
        let pidless = register("browser-agent", None).await;
        let reply = orphan(&daemon.socket, claim(&pidless.id, "browser")).await;
        assert!(held(&reply), "{reply:?}");
        let z_shell = shell(sh, &daemon.socket);
        register("agent-z", Some(z_shell.id())).await;
        let reply = send(z_shell, claim(&pidless.id, "from-z")).await;
        assert!(refused(&reply), "{reply:?}");

        // The daemon's own process is trusted; a sender that cannot be
        // identified is not.
        let mut own = Request::Claim {
            agent: x.id.to_string(),
            resource: format!("path:{}/own", tmp.path().display()),
            mode: Default::default(),
            amount: None,
            ttl_secs: 60,
            note: None,
            wait_secs: 0,
            automatic: false,
        };
        let me = Some((std::process::id(), procinfo::start_time(std::process::id())));
        assert!(daemon.agent_refusal(me, &mut own).await.is_none());
        assert!(
            daemon
                .agent_refusal(None, &mut own)
                .await
                .is_some_and(|reply| refused(&reply))
        );
        serving.abort();

        // With the check off, Y's process acts as X.
        let off = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::open(off.path().into(), off.path().join("host.sock")).unwrap();
        daemon.person_check = false;
        daemon.agent_check = false;
        let daemon = Arc::new(daemon);
        let listener = crate::server::bind(&daemon).await.unwrap();
        let serving = tokio::spawn(crate::server::serve(daemon.clone(), listener));
        let mut x_shell = shell(sh, &daemon.socket);
        let y_shell = shell(sh, &daemon.socket);
        let mut ids = Vec::new();
        for (name, pid) in [("agent-x", x_shell.id()), ("agent-y", y_shell.id())] {
            let Response::Agent { agent } = daemon
                .handle(Request::Register {
                    spec: AgentSpec {
                        name: name.into(),
                        workdir: Some(off.path().to_owned()),
                        ..AgentSpec::default()
                    },
                    pid: Some(pid),
                    session: None,
                })
                .await
            else {
                panic!("registers")
            };
            ids.push(agent.id);
        }
        let reply = send(
            y_shell,
            serde_json::json!({
                "op": "claim",
                "agent": ids[0],
                "resource": format!("path:{}/switched-off", off.path().display()),
                "ttl_secs": 60,
            })
            .to_string(),
        )
        .await;
        assert!(held(&reply), "{reply:?}");
        let _ = x_shell.kill();
        let _ = x_shell.wait();
        serving.abort();
    }
}
