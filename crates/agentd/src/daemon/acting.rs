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
//! under side by side — or, for a Codex agent, the sender runs under its
//! profile's shared app-server, whose conversations its process tree cannot
//! tell apart, proven by its kernel image in the profile's package cache and
//! by serving the profile's control socket, never by its arguments. The
//! person's: outside every agent's session, as the person check traces it
//! (the app, their own terminal). The kernel says which process sent the
//! request; the request cannot.
//!
//! So an agent's hooks, its MCP server, a shell its tools opened and every
//! session those started act as it, and the person may act as any agent;
//! from inside another agent's session, or from a process whose parents
//! cannot be read, the request is refused `identity_mismatch`. The person
//! (`user`, or their record) is the person check's; the daemon's own process
//! is trusted; and a harness's own daemon started with
//! `AGENTDOCKER_AGENT_CHECK=off` skips the check.
//!
//! A registration's `pid` is the same kind of claim: the process it names
//! becomes an agent's own, the anchor of its session here. So the sender
//! must be able to name it ([`naming`]): itself or one of its parents (a
//! hook names its provider, an MCP server its host), or a process it
//! started (a launcher names its terminal); a process a live agent already
//! holds only from inside that agent's session, as a request acting as it
//! would be sent, which is what lets a Codex terminal's companion launcher's
//! other children name it; and a Codex terminal resuming the conversation
//! the spec names from under that profile's proven shared server, where a
//! terminal conversation's hooks and MCP servers run. Otherwise only the
//! person may, and the registration is refused `identity_mismatch`. The
//! same switch turns it off.

use super::*;
use agentdocker_core::HUMAN;
use agentdocker_host::codex_control;
use agentdocker_host::procinfo::{self, Unseen};
use person::Peer;

/// How far up a sender's parents are walked.
const ANCESTRY: usize = 64;
/// How long asking a Codex profile's control socket who serves it may take.
const SERVER_CHECK: std::time::Duration = std::time::Duration::from_secs(2);

/// One process, by its pid and its birth.
type Process = (u32, DateTime<Utc>);

/// What the kernel says of processes, as the walks here read it: the
/// host's own ([`Kernel::host`]), or a test's stand-in.
struct Kernel<'a> {
    /// A process's parent, and its own birth.
    step: &'a dyn Fn(u32) -> Option<(u32, DateTime<Utc>)>,
    /// Why a process could not be read.
    unseen: &'a dyn Fn(u32) -> Unseen,
    /// A process's own arguments.
    arguments: &'a dyn Fn(u32) -> Option<Vec<String>>,
    /// The kernel image a process runs.
    executable: &'a dyn Fn(u32) -> Option<std::path::PathBuf>,
    /// A process's birth.
    born: &'a dyn Fn(u32) -> Option<DateTime<Utc>>,
    /// Whether an executable is this daemon's companion CLI.
    companion: &'a dyn Fn(&std::path::Path) -> bool,
}

fn executable_of(pid: u32) -> Option<std::path::PathBuf> {
    procinfo::executable_path_of(pid).ok()
}

fn is_companion(executable: &std::path::Path) -> bool {
    person::companion_cli()
        .is_some_and(|cli| agentdocker_host::program::same_file(&cli, executable))
}

impl Kernel<'static> {
    /// This host's kernel.
    fn host() -> Self {
        Self {
            step: &procinfo::parent_and_birth,
            unseen: &procinfo::unseen,
            arguments: &procinfo::arguments,
            executable: &executable_of,
            born: &procinfo::start_time,
            companion: &is_companion,
        }
    }
}

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
        // The witness token and the session of one of the worktree's
        // owners, which the daemon finds from the sender and names in it
        // (`authenticate_report`), speak for a reported commit.
        | R::ReportCommit { .. }
        // The exact live owner process these name is checked by them.
        | R::OpenSecretReview { .. }
        | R::PollSecretReview { .. }
        | R::CodeVerdict { .. }
        // The process a registration names has a check of its own
        // (`register_refusal`).
        | R::Register { .. }
        // What is acted on, not who acts.
        | R::WorktreeRemove { .. }
        | R::GrantAccess { .. }
        | R::Import { .. }
        | R::Run { .. }
        | R::ResumeSession { .. }
        | R::RunContainer { .. }
        | R::RestartContainer { .. }
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

/// The processes among `chain` that may be `profile`'s shared Codex server
/// (`profile` canonical): each one whose kernel image (`executable`, never
/// its arguments, which any process may set) is a Codex binary in that
/// profile's own package cache ([`codex_control::shared_server_profile`]),
/// while it is still the process walked (`born` reads a birth again).
/// Which of them, if any, really is the server is for the profile's control
/// socket to say ([`codex_control::serving_process`]).
fn shared_server_candidates(
    chain: &[Process],
    profile: &std::path::Path,
    executable: impl Fn(u32) -> Option<std::path::PathBuf>,
    born: impl Fn(u32) -> Option<DateTime<Utc>>,
) -> Vec<Process> {
    chain
        .iter()
        .copied()
        .filter(|(pid, birth)| {
            executable(*pid)
                .and_then(|image| std::fs::canonicalize(image).ok())
                .and_then(|image| codex_control::shared_server_profile(&image))
                .and_then(|named| std::fs::canonicalize(named).ok())
                .is_some_and(|named| named == profile)
                && born(*pid) == Some(*birth)
        })
        .collect()
}

/// Whether the process serving the profile's control socket (`serving`, as
/// the kernel names it) is one of the `candidates` among the sender's
/// parents, still born when it was walked.
fn served_by(
    candidates: &[Process],
    serving: Option<u32>,
    born: impl Fn(u32) -> Option<DateTime<Utc>>,
) -> bool {
    serving.is_some_and(|pid| {
        candidates
            .iter()
            .any(|(candidate, birth)| *candidate == pid && born(pid) == Some(*birth))
    })
}

/// Where a sender was found to run, before the person check's trace.
enum Session {
    /// Inside the agent's session.
    Inside,
    /// Not under any of the agent's own processes. For a Codex agent of a
    /// known profile, the parents that may be that profile's shared server.
    Outside { shared: Vec<Process> },
}

/// What is known of a Codex agent for finding its session: its terminal
/// process, when it has one, and its provider profile, canonical, when its
/// record names one.
#[derive(Clone)]
struct Codex {
    terminal: Option<Process>,
    profile: Option<std::path::PathBuf>,
}

/// Where the sender of a request acting as an agent whose own processes are
/// `anchors` runs: inside that agent's session when one of them stands
/// among its parents, or, for a Codex agent, when its terminal's companion
/// launcher does. Otherwise, for a Codex agent of a known profile, which of
/// its parents may be that profile's shared server.
fn find_session(
    sender: u32,
    born: DateTime<Utc>,
    anchors: &[Process],
    codex: Option<&Codex>,
    kernel: &Kernel,
) -> Session {
    let (chain, end) = lineage(sender, born, kernel.step, kernel.unseen, |process| {
        anchors.contains(&process)
    });
    if end == End::Found {
        return Session::Inside;
    }
    let Some(codex) = codex else {
        return Session::Outside { shared: Vec::new() };
    };
    let launched = codex
        .terminal
        .and_then(|terminal| {
            codex_launcher(
                terminal,
                kernel.step,
                kernel.arguments,
                kernel.executable,
                kernel.companion,
            )
        })
        .is_some_and(|launcher| chain.contains(&launcher));
    if launched {
        return Session::Inside;
    }
    Session::Outside {
        shared: codex.profile.as_deref().map_or_else(Vec::new, |profile| {
            shared_server_candidates(&chain, profile, kernel.executable, kernel.born)
        }),
    }
}

/// What the request acts as, read under the lock.
#[derive(Clone)]
struct Acting {
    id: AgentId,
    name: String,
    /// Its own processes: its process, its session owner, its input
    /// controller (bound, or launched and not yet bound) and the provider
    /// process the binding names.
    anchors: Vec<Process>,
    /// For a Codex agent, what finds its launcher and its shared server.
    codex: Option<Codex>,
}

impl Acting {
    /// What finds `record`'s session, as the record says it.
    fn of(record: &AgentRecord) -> Self {
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
        Self {
            id: record.id.clone(),
            name: record.spec.name.clone(),
            codex: (record.spec.runtime == "codex").then(|| Codex {
                terminal: own,
                profile: record
                    .spec
                    .labels
                    .get(agentdocker_core::agent::PROFILE_LABEL)
                    .map(std::path::PathBuf::from),
            }),
            anchors,
        }
    }
}

/// What a registration's spec says of the process it names when that is a
/// Codex terminal whose conversation runs in a profile's shared server: the
/// profile, canonical, and the conversation's thread.
struct Terminal {
    profile: std::path::PathBuf,
    thread: String,
}

impl Terminal {
    /// From a `codex` spec that names both its profile and its thread; the
    /// profile is resolved on the filesystem, so this is not for the lock.
    fn of(spec: &AgentSpec) -> Option<Self> {
        if spec.runtime != "codex" {
            return None;
        }
        let thread = spec.labels.get("session_id").filter(|t| !t.is_empty())?;
        let profile = spec.labels.get(agentdocker_core::agent::PROFILE_LABEL)?;
        Some(Self {
            profile: std::fs::canonicalize(profile).ok()?,
            thread: thread.clone(),
        })
    }
}

/// What lets a registration's sender name the process it names.
#[derive(Debug, PartialEq, Eq)]
enum Naming {
    /// Its relation to the sender does.
    Admitted,
    /// It would, if one of a profile's candidates for its shared server,
    /// each among the sender's parents, serves that profile's control
    /// socket ([`served_by`]).
    Served(Vec<(std::path::PathBuf, Vec<Process>)>),
    /// Nothing about it does; the person check's trace decides.
    Unrelated,
}

/// Whether the process a registration names (`named`, by pid and its birth
/// now) is one its sender (`sender`) may name, as the kernel tells it. One
/// that live agents hold as their own (`holders`, their sessions as
/// [`Acting::of`] finds them) may be named from inside one of their
/// sessions, as a request acting as that agent may be sent ([`find_session`],
/// its Codex launcher and shared server included), and from nowhere else
/// here: registering it is acting as that agent. One nobody holds may be
/// named when it is the sender or one of its parents (the sender runs inside
/// it: a hook naming its provider, an MCP server its host), or when the
/// sender stands among its own parents (the sender started it: a launcher
/// naming its terminal). Either way, a Codex terminal whose own arguments
/// resume the thread `terminal` names may be named from under that profile's
/// shared server, which runs a terminal conversation's hooks and MCP servers
/// beside rather than under it, when every live agent that holds it is a
/// Codex agent of no other profile; the server is for its control socket to
/// prove ([`Naming::Served`]).
fn naming(
    sender: Process,
    named: Process,
    holders: &[Acting],
    terminal: Option<&Terminal>,
    kernel: &Kernel,
) -> Naming {
    let (pid, born) = sender;
    let mut served = Vec::new();
    if holders.is_empty() {
        let (_, up) = lineage(pid, born, kernel.step, kernel.unseen, |p| p == named);
        let started = || {
            let (named_pid, named_born) = named;
            lineage(named_pid, named_born, kernel.step, kernel.unseen, |p| {
                p == sender
            })
            .1 == End::Found
        };
        if up == End::Found || started() {
            return Naming::Admitted;
        }
    }
    for holder in holders {
        match find_session(pid, born, &holder.anchors, holder.codex.as_ref(), kernel) {
            Session::Inside => return Naming::Admitted,
            Session::Outside { shared } if !shared.is_empty() => {
                if let Some(profile) = holder.codex.as_ref().and_then(|c| c.profile.clone()) {
                    served.push((profile, shared));
                }
            }
            Session::Outside { .. } => {}
        }
    }
    if let Some(terminal) = terminal {
        let theirs = holders.iter().all(|holder| {
            holder.codex.as_ref().is_some_and(|codex| {
                codex
                    .profile
                    .as_ref()
                    .is_none_or(|profile| *profile == terminal.profile)
            })
        });
        let (named_pid, named_born) = named;
        let resumes = (kernel.born)(named_pid) == Some(named_born)
            && (kernel.arguments)(named_pid)
                .is_some_and(|argv| procinfo::codex_resumes(&argv, &terminal.thread));
        if theirs && resumes {
            let (chain, _) = lineage(pid, born, kernel.step, kernel.unseen, |_| false);
            let shared =
                shared_server_candidates(&chain, &terminal.profile, kernel.executable, kernel.born);
            if !shared.is_empty() {
                served.push((terminal.profile.clone(), shared));
            }
        }
    }
    if served.is_empty() {
        Naming::Unrelated
    } else {
        Naming::Served(served)
    }
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
            let acting = Acting::of(record);
            *field = acting.id.to_string();
            acting
        };
        if let Some((sender, Some(born))) = peer {
            let anchors = acting.anchors.clone();
            let codex = acting.codex.clone().map(|mut codex| {
                codex.profile = codex
                    .profile
                    .and_then(|profile| std::fs::canonicalize(profile).ok());
                codex
            });
            let profile = codex.as_ref().and_then(|codex| codex.profile.clone());
            let found = tokio::task::spawn_blocking(move || {
                find_session(sender, born, &anchors, codex.as_ref(), &Kernel::host())
            })
            .await
            .unwrap_or(Session::Outside { shared: Vec::new() });
            match found {
                Session::Inside => return None,
                // A Codex terminal conversation's hooks and MCP servers run
                // under its profile's shared server, whose conversations its
                // process tree cannot tell apart: the server is proven by
                // its image and by serving the profile's control socket,
                // never by what its arguments say.
                Session::Outside { shared } if !shared.is_empty() => {
                    if let Some(profile) = profile {
                        let serving = tokio::time::timeout(
                            SERVER_CHECK,
                            codex_control::serving_process(&profile),
                        )
                        .await
                        .ok()
                        .and_then(Result::ok)
                        .flatten();
                        if served_by(&shared, serving, procinfo::start_time) {
                            return None;
                        }
                    }
                }
                Session::Outside { .. } => {}
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

    /// A refusal when `request` registers a process (`register`'s `pid`)
    /// that `peer`, the process on the socket, may not name ([`naming`]),
    /// and `peer` is not the person's either: the person may register any
    /// process. Follows the agent check's switch, and trusts the daemon's
    /// own process, as [`Daemon::agent_refusal`] does. `None` lets it
    /// through.
    pub(crate) async fn register_refusal(
        self: &Arc<Self>,
        peer: Option<Peer>,
        request: &Request,
    ) -> Option<Response> {
        let Request::Register {
            spec,
            pid: Some(pid),
            ..
        } = request
        else {
            return None;
        };
        if !self.agent_check || peer.is_some_and(|(sender, _)| sender == std::process::id()) {
            return None;
        }
        let pid = *pid;
        let live: Vec<Acting> = lock(&self.state).registry.live().map(Acting::of).collect();
        let spec = spec.clone();
        let identified = peer.and_then(|(sender, born)| born.map(|born| (sender, born)));
        let (found, holder) = tokio::task::spawn_blocking(move || {
            let named = procinfo::start_time(pid).map(|born| (pid, born));
            let holders: Vec<Acting> = named
                .map(|named| {
                    live.into_iter()
                        .filter(|holder| holder.anchors.contains(&named))
                        .map(|mut holder| {
                            if let Some(codex) = &mut holder.codex {
                                codex.profile = codex
                                    .profile
                                    .take()
                                    .map(|p| std::fs::canonicalize(&p).unwrap_or(p));
                            }
                            holder
                        })
                        .collect()
                })
                .unwrap_or_default();
            let found = identified.zip(named).map(|(sender, named)| {
                naming(
                    sender,
                    named,
                    &holders,
                    Terminal::of(&spec).as_ref(),
                    &Kernel::host(),
                )
            });
            (found, holders.into_iter().next())
        })
        .await
        .unwrap_or((None, None));
        match found {
            Some(Naming::Admitted) => return None,
            Some(Naming::Served(servers)) => {
                for (profile, candidates) in servers {
                    let serving = tokio::time::timeout(
                        SERVER_CHECK,
                        codex_control::serving_process(&profile),
                    )
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .flatten();
                    if served_by(&candidates, serving, procinfo::start_time) {
                        return None;
                    }
                }
            }
            Some(Naming::Unrelated) | None => {}
        }
        let why = match self.trace(peer, request).await {
            person::Traced::Person => return None,
            person::Traced::Inside(reason) => format!("it was sent from inside {reason}"),
            person::Traced::Unknown(reason) => format!("{reason} cannot be the person's"),
        };
        let sender = peer.map_or(0, |(sender, _)| sender);
        warn!(peer = sender, pid, %why, "refused a registration of a process its sender may not name");
        Some(naming_refusal(pid, holder.as_ref(), &why))
    }
}

fn naming_refusal(pid: u32, holder: Option<&Acting>, why: &str) -> Response {
    const RULE: &str = "a process registers itself, one of its parents or a process it started, \
                        and an agent's process only from inside that agent's session; the person \
                        may register any";
    let (message, details) = match holder {
        Some(holder) => (
            format!(
                "this registers process {pid}, agent {}'s own, and {why}; {RULE}",
                holder.name
            ),
            json!({"pid": pid, "agent": holder.id, "name": holder.name}),
        ),
        None => (
            format!(
                "this registers process {pid}, which is neither its sender, one of the sender's \
                 parents nor a process the sender started, and {why}; {RULE}"
            ),
            json!({"pid": pid}),
        ),
    };
    Response::Error {
        code: ErrorCode::IdentityMismatch,
        message,
        details: Some(details),
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
            serde_json::json!({"op": "report_commit", "checkout": "/w", "token": "t", "head": "abc", "agent": "x"}),
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

    /// Codex's shared server is found by its kernel image in the agent's
    /// own profile's package cache, never by its arguments, and only while
    /// it is still the process walked; and it counts only when it is the
    /// process serving that profile's control socket.
    #[test]
    fn codex_s_shared_server_is_proven_by_its_image_and_its_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = std::fs::canonicalize(tmp.path()).unwrap().join("profile");
        let other = profile.with_file_name("other");
        let image = |profile: &std::path::Path| {
            let image = profile.join("packages/app-server-daemon/releases/0.162.0/bin/codex");
            std::fs::create_dir_all(image.parent().unwrap()).unwrap();
            std::fs::write(&image, "").unwrap();
            image
        };
        let (ours, theirs) = (image(&profile), image(&other));
        let python = tmp.path().join("python3");
        std::fs::write(&python, "").unwrap();
        // 40 is the sender; 30 runs Python with a shared server's arguments;
        // 20 is this profile's server and 10 another profile's.
        let chain = [(40, at(40)), (30, at(30)), (20, at(20)), (10, at(10))];
        let executable = |pid: u32| match pid {
            30 => Some(python.clone()),
            20 => Some(ours.clone()),
            10 => Some(theirs.clone()),
            _ => None,
        };
        let born = |pid: u32| Some(at(i64::from(pid)));
        assert_eq!(
            shared_server_candidates(&chain, &profile, executable, born),
            [(20, at(20))]
        );
        assert!(
            shared_server_candidates(&chain, &profile, executable, |_| Some(at(1))).is_empty(),
            "another process has its pid now"
        );
        let found = [(20, at(20))];
        assert!(served_by(&found, Some(20), born));
        assert!(!served_by(&found, Some(30), born), "another serves it");
        assert!(!served_by(&found, None, born), "nobody serves it");
        assert!(!served_by(&found, Some(20), |_| Some(at(2))), "reused");
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
        // perhaps run inside an agent's session, leaves aside. The agent
        // check is on whatever the environment says, since its refusals are
        // what is tested.
        daemon.person_check = false;
        daemon.agent_check = true;
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

    /// The stand-in for Codex's shared server in
    /// [`a_shared_codex_server_counts_only_by_its_image_and_its_socket`]:
    /// this test binary, copied into a profile's package cache, run by name.
    /// It serves the profile's control socket when told to, then starts the
    /// client that sends the request, and prints its answer.
    #[cfg(unix)]
    #[test]
    #[ignore = "run only as the shared server's stand-in"]
    fn shared_server_stand_in() {
        let Some(told) = std::env::var_os("AGENTDOCKER_TEST_STAND_IN") else {
            return;
        };
        let told: serde_json::Value = serde_json::from_str(&told.to_string_lossy()).unwrap();
        let _serving = told["socket"]
            .as_str()
            .map(|socket| std::os::unix::net::UnixListener::bind(socket).unwrap());
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(told["client"].as_str().unwrap())
            .arg(told["daemon"].as_str().unwrap())
            .arg(told["request"].as_str().unwrap())
            .output()
            .unwrap();
        println!("reply:{}", String::from_utf8_lossy(&output.stdout).trim());
    }

    /// Over the real socket: a request acting as a Codex agent from under
    /// its profile's shared server is answered only when that server is
    /// proven by its kernel image in the profile's package cache and by
    /// serving the profile's control socket. A process that only says it
    /// is one — another agent's child running Python as `codex app-server
    /// --managed-daemon` — is refused, as is a Codex image in the cache that
    /// serves nothing.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_shared_codex_server_counts_only_by_its_image_and_its_socket() {
        use std::io::Write as _;
        let tmp = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::open(tmp.path().into(), tmp.path().join("host.sock")).unwrap();
        daemon.person_check = false;
        daemon.agent_check = true;
        let daemon = Arc::new(daemon);
        let listener = crate::server::bind(&daemon).await.unwrap();
        let serving = tokio::spawn(crate::server::serve(daemon.clone(), listener));
        // A socket path may have only 104 bytes on macOS.
        let short = tempfile::Builder::new()
            .prefix("ac-")
            .tempdir_in(if cfg!(target_os = "macos") {
                std::path::PathBuf::from("/tmp")
            } else {
                std::env::temp_dir()
            })
            .unwrap();
        let profile = std::fs::canonicalize(short.path()).unwrap().join("p");
        std::fs::create_dir_all(profile.join(codex_control::DIRECTORY)).unwrap();
        let image = profile.join("packages/app-server-daemon/releases/0.162.0/bin/codex");
        std::fs::create_dir_all(image.parent().unwrap()).unwrap();
        std::fs::copy(std::env::current_exe().unwrap(), &image).unwrap();
        let Response::Agent { agent: codex } = daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "codex-conversation".into(),
                    runtime: "codex".into(),
                    workdir: Some(tmp.path().to_owned()),
                    labels: [(
                        agentdocker_core::agent::PROFILE_LABEL.to_owned(),
                        profile.display().to_string(),
                    )]
                    .into(),
                    ..AgentSpec::default()
                },
                pid: None,
                session: None,
            })
            .await
        else {
            panic!("registers")
        };
        let client = "import socket,sys\ns=socket.socket(socket.AF_UNIX)\ns.connect(sys.argv[-2])\ns.sendall(sys.argv[-1].encode()+b'\\n')\nprint(s.makefile().readline(),end='',flush=True)\n";
        let claim = |resource: &str| {
            serde_json::json!({
                "op": "claim",
                "agent": codex.id,
                "resource": format!("path:{}/{resource}", tmp.path().display()),
                "ttl_secs": 60,
            })
            .to_string()
        };
        let reply = |stdout: &[u8], prefix: &str| -> Response {
            let text = String::from_utf8_lossy(stdout);
            let line = text
                .lines()
                .find_map(|line| line.split_once(prefix).map(|(_, reply)| reply))
                .unwrap_or_else(|| panic!("no reply in {text}"));
            serde_json::from_str(line).unwrap()
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

        // Another agent's child that runs Python as a shared server.
        let spoof = "import subprocess,sys\nprint('reply:'+subprocess.run(['codex','-c',sys.argv[1],'app-server','--managed-daemon',sys.argv[2],sys.argv[3]],executable=sys.executable,capture_output=True,text=True).stdout,flush=True)\n";
        let mut other = std::process::Command::new("sh")
            .arg("-c")
            .arg("read request; python3 -c \"$0\" \"$1\" \"$2\" \"$request\"; exit $?")
            .arg(spoof)
            .arg(client)
            .arg(&daemon.socket)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let Response::Agent { .. } = daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "another-agent".into(),
                    workdir: Some(tmp.path().to_owned()),
                    ..AgentSpec::default()
                },
                pid: Some(other.id()),
                session: None,
            })
            .await
        else {
            panic!("registers")
        };
        writeln!(other.stdin.take().unwrap(), "{}", claim("spoofed")).unwrap();
        let output = tokio::task::spawn_blocking(move || other.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        let answer = reply(&output.stdout, "reply:");
        assert!(refused(&answer), "{answer:?}");

        // The stand-in, a Codex image in the profile's cache: answered only
        // while it serves the profile's control socket.
        let stand_in = |socket: Option<&std::path::Path>, request: String| {
            let told = serde_json::json!({
                "socket": socket,
                "client": client,
                "daemon": daemon.socket,
                "request": request,
            });
            let image = image.clone();
            async move {
                tokio::task::spawn_blocking(move || {
                    std::process::Command::new(&image)
                        .args([
                            "daemon::acting::tests::shared_server_stand_in",
                            "--exact",
                            "--ignored",
                            "--nocapture",
                            "--test-threads=1",
                        ])
                        .env("AGENTDOCKER_TEST_STAND_IN", told.to_string())
                        .output()
                        .unwrap()
                })
                .await
                .unwrap()
            }
        };
        let output = stand_in(None, claim("unserved")).await;
        let answer = reply(&output.stdout, "reply:");
        assert!(
            refused(&answer),
            "a Codex image that serves nothing: {answer:?}"
        );
        let socket = codex_control::socket_path(&profile);
        let output = stand_in(Some(&socket), claim("served")).await;
        let answer = reply(&output.stdout, "reply:");
        assert!(
            matches!(answer, Response::Lease { .. }),
            "the profile's server: {answer:?}"
        );
        assert_eq!(lock(&daemon.state).leases.len(), 1);
        serving.abort();
    }

    /// Processes as a fake kernel reads them: each pid's parent, its birth
    /// (`at(pid)`, so a parent is older than its child), its arguments and
    /// its image. `/opt/agentdocker` is the companion CLI.
    struct Table {
        parents: HashMap<u32, u32>,
        argv: HashMap<u32, Vec<String>>,
        images: HashMap<u32, std::path::PathBuf>,
    }

    impl Table {
        fn naming(
            &self,
            sender: u32,
            named: u32,
            holders: &[Acting],
            terminal: Option<&Terminal>,
        ) -> Naming {
            let step = |pid: u32| {
                self.parents
                    .get(&pid)
                    .map(|parent| (*parent, at(i64::from(pid))))
            };
            let born = |pid: u32| self.parents.contains_key(&pid).then(|| at(i64::from(pid)));
            let arguments = |pid: u32| self.argv.get(&pid).cloned();
            let executable = |pid: u32| self.images.get(&pid).cloned();
            let companion = |exe: &std::path::Path| exe == std::path::Path::new("/opt/agentdocker");
            let kernel = Kernel {
                step: &step,
                unseen: &|_: u32| Unseen::Ended,
                arguments: &arguments,
                executable: &executable,
                born: &born,
                companion: &companion,
            };
            naming(
                (sender, at(i64::from(sender))),
                (named, at(i64::from(named))),
                holders,
                terminal,
                &kernel,
            )
        }
    }

    /// A live record of `runtime` whose own process is `pid`, as
    /// [`Acting::of`] finds its session.
    fn holder(runtime: &str, pid: u32, profile: Option<&std::path::Path>) -> Acting {
        let mut record = AgentRecord::new(
            AgentSpec {
                name: format!("{runtime}-{pid}"),
                runtime: runtime.into(),
                labels: profile
                    .map(|profile| {
                        (
                            agentdocker_core::agent::PROFILE_LABEL.to_owned(),
                            profile.display().to_string(),
                        )
                    })
                    .into_iter()
                    .collect(),
                ..AgentSpec::default()
            },
            false,
            at(0),
        );
        record.pid = Some(pid);
        record.process_started_at = Some(at(i64::from(pid)));
        Acting::of(&record)
    }

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    const THREAD: &str = "019a0000-0000-7000-8000-000000000001";

    /// The process tree every naming case below reads: the person's shell
    /// (10) has started Claude Code (20, agent X) with its MCP server (21)
    /// and a hook (23, under a shell 22); the companion CLI's `codex-native`
    /// (30), which started a Codex terminal (31, agent Y) and beside it the
    /// dedicated server (32) a hook (33) runs under; agent Z (40), whose tool
    /// shell (41) started agent W (42); and a launcher (50) with its child
    /// (51). Profile P's shared server (60) runs an MCP server (61), profile
    /// Q's (70) another (71); in another terminal (80) Codex resumes
    /// [`THREAD`] (81), and another conversation (82).
    fn table(images: &std::path::Path) -> (Table, std::path::PathBuf, std::path::PathBuf) {
        let profile = |name: &str| {
            let profile = images.join(name);
            let image = profile.join("packages/app-server-daemon/releases/0.162.0/bin/codex");
            std::fs::create_dir_all(image.parent().unwrap()).unwrap();
            std::fs::write(&image, "").unwrap();
            (profile, image)
        };
        let ((p, p_image), (q, q_image)) = (profile("p"), profile("q"));
        let parents = HashMap::from([
            (10, 1),
            (20, 10),
            (21, 20),
            (22, 20),
            (23, 22),
            (30, 10),
            (31, 30),
            (32, 30),
            (33, 32),
            (40, 10),
            (41, 40),
            (42, 41),
            (50, 10),
            (51, 50),
            (60, 1),
            (61, 60),
            (70, 1),
            (71, 70),
            (80, 1),
            (81, 80),
            (82, 80),
        ]);
        let argv = HashMap::from([
            (
                30,
                argv(&["agentdocker", "codex-native", "--program", "/opt/codex"]),
            ),
            (31, argv(&["/opt/codex", "--remote", "ws://127.0.0.1:1"])),
            (81, argv(&["codex", "resume", THREAD])),
            (
                82,
                argv(&["codex", "resume", "019a0000-0000-7000-8000-000000000002"]),
            ),
        ]);
        let images = HashMap::from([
            (30, std::path::PathBuf::from("/opt/agentdocker")),
            (60, p_image),
            (70, q_image),
        ]);
        (
            Table {
                parents,
                argv,
                images,
            },
            p,
            q,
        )
    }

    /// A registration names a process nobody holds when it is its sender (a
    /// client registering itself) or one of its sender's parents (a hook
    /// naming its provider, an MCP server its host), or when its sender
    /// started it (a launcher naming its child, and the Codex launcher
    /// naming the terminal it started). Nothing else is admitted by ancestry:
    /// not a sibling, not a stranger, not a pid reused since.
    #[test]
    fn an_unheld_process_is_named_by_itself_its_children_or_its_starter() {
        let tmp = tempfile::tempdir().unwrap();
        let (table, _, _) = table(&std::fs::canonicalize(tmp.path()).unwrap());
        // The peer itself.
        assert_eq!(table.naming(21, 21, &[], None), Naming::Admitted);
        // One of its parents: an MCP server its host, a hook its provider
        // past the shell between them.
        assert_eq!(table.naming(21, 20, &[], None), Naming::Admitted);
        assert_eq!(table.naming(23, 20, &[], None), Naming::Admitted);
        // A process it started, directly or below a child.
        assert_eq!(table.naming(50, 51, &[], None), Naming::Admitted);
        assert_eq!(table.naming(20, 23, &[], None), Naming::Admitted);
        // The Codex launcher names the terminal it started.
        assert_eq!(table.naming(30, 31, &[], None), Naming::Admitted);
        // A sibling, a cousin or a stranger is not.
        assert_eq!(table.naming(21, 22, &[], None), Naming::Unrelated);
        assert_eq!(table.naming(41, 51, &[], None), Naming::Unrelated);
        assert_eq!(table.naming(41, 20, &[], None), Naming::Unrelated);
        // A process given the named pid since is not the one walked.
        let step = |pid: u32| table.parents.get(&pid).map(|p| (*p, at(i64::from(pid))));
        let born = |pid: u32| Some(at(i64::from(pid)));
        let kernel = Kernel {
            step: &step,
            unseen: &|_: u32| Unseen::Ended,
            arguments: &|_: u32| -> Option<Vec<String>> { None },
            executable: &|_: u32| -> Option<std::path::PathBuf> { None },
            born: &born,
            companion: &|_: &std::path::Path| false,
        };
        assert_eq!(
            naming((21, at(21)), (20, at(19)), &[], None, &kernel),
            Naming::Unrelated
        );
    }

    /// A process a live agent holds is named only from inside that agent's
    /// session, as a request acting as it would be sent: from under its own
    /// process, or, for a Codex terminal the companion launcher started,
    /// from anything else that launcher started (a hook of the dedicated
    /// server beside it). An impostor cannot name it to take the identity
    /// over: not another agent's process, not one under it, and not the
    /// process that started it, which would have been admitted had nobody
    /// held it.
    #[test]
    fn a_held_process_is_named_only_from_inside_its_agent_s_session() {
        let tmp = tempfile::tempdir().unwrap();
        let (table, _, _) = table(&std::fs::canonicalize(tmp.path()).unwrap());
        let x = [holder("claude-code", 20, None)];
        assert_eq!(table.naming(23, 20, &x, None), Naming::Admitted, "X's hook");
        assert_eq!(table.naming(20, 20, &x, None), Naming::Admitted, "X itself");
        // A process a recorded agent's launcher started: the Codex
        // terminal Y, re-registered from under the server its launcher
        // started beside it.
        let y = [holder("codex", 31, None)];
        assert_eq!(table.naming(33, 31, &y, None), Naming::Admitted);
        assert_eq!(table.naming(30, 31, &y, None), Naming::Admitted);
        // Impostors.
        assert_eq!(table.naming(41, 20, &x, None), Naming::Unrelated);
        assert_eq!(table.naming(40, 20, &x, None), Naming::Unrelated);
        assert_eq!(table.naming(41, 31, &y, None), Naming::Unrelated);
        assert_eq!(table.naming(21, 31, &y, None), Naming::Unrelated);
        // Z started W, and still may not take W's identity.
        let w = [holder("claude-code", 42, None)];
        assert_eq!(table.naming(41, 42, &[], None), Naming::Admitted);
        assert_eq!(table.naming(41, 42, &w, None), Naming::Unrelated);
        assert_eq!(table.naming(10, 42, &w, None), Naming::Unrelated);
    }

    /// From under a profile's shared server, a Codex terminal whose own
    /// arguments resume the conversation the spec names may be named as a
    /// record of that profile, when the server among the sender's parents
    /// is a Codex image in that profile's package cache, and then only if
    /// it serves the profile's control socket ([`Naming::Served`]). Not a
    /// terminal of another conversation, not from another profile's server,
    /// not a process another profile's agent or another runtime's holds,
    /// and not a terminal from an ordinary process.
    #[test]
    fn a_shared_server_names_the_terminal_that_resumes_its_conversation() {
        let tmp = tempfile::tempdir().unwrap();
        let (table, p, q) = table(&std::fs::canonicalize(tmp.path()).unwrap());
        let terminal = |profile: &std::path::Path, thread: &str| Terminal {
            profile: profile.to_owned(),
            thread: thread.to_owned(),
        };
        let theirs = terminal(&p, THREAD);
        let served = Naming::Served(vec![(p.clone(), vec![(60, at(60))])]);
        assert_eq!(table.naming(61, 81, &[], Some(&theirs)), served);
        // Held by a Codex record of the same profile, or of none.
        for held in [holder("codex", 81, Some(&p)), holder("codex", 81, None)] {
            assert!(matches!(
                table.naming(61, 81, &[held], Some(&theirs)),
                Naming::Served(_)
            ));
        }
        // Another conversation's terminal, or a spec naming another thread.
        assert_eq!(table.naming(61, 82, &[], Some(&theirs)), Naming::Unrelated);
        // From another profile's server, or naming this terminal for it.
        assert_eq!(table.naming(71, 81, &[], Some(&theirs)), Naming::Unrelated);
        assert_eq!(
            table.naming(61, 81, &[], Some(&terminal(&q, THREAD))),
            Naming::Unrelated
        );
        // Held by another profile's agent, or another runtime's.
        for held in [
            holder("codex", 81, Some(&q)),
            holder("claude-code", 81, None),
        ] {
            assert_eq!(
                table.naming(61, 81, &[held], Some(&theirs)),
                Naming::Unrelated
            );
        }
        // From an ordinary process, or without a terminal's claim.
        assert_eq!(table.naming(41, 81, &[], Some(&theirs)), Naming::Unrelated);
        assert_eq!(table.naming(61, 81, &[], None), Naming::Unrelated);
    }

    /// A client for the registration tests: registers `spec` naming the
    /// process the mode says (`self`, its `parent`, a `child` it starts and
    /// keeps until the answer, or a pid) and prints the answer.
    #[cfg(unix)]
    const REGISTRAR: &str = "import json,os,socket,subprocess,sys\nmode=sys.argv[2]\nchild=None\nif mode=='self':\n    pid=os.getpid()\nelif mode=='parent':\n    pid=os.getppid()\nelif mode=='child':\n    child=subprocess.Popen([sys.executable,'-c','import time\\ntime.sleep(30)'])\n    pid=child.pid\nelse:\n    pid=int(mode)\ns=socket.socket(socket.AF_UNIX)\ns.connect(sys.argv[1])\ns.sendall(json.dumps({'op':'register','spec':json.loads(sys.argv[3]),'pid':pid}).encode()+b'\\n')\nprint(s.makefile().readline(),end='',flush=True)\nif child:\n    child.kill()\n    child.wait()\n";

    /// Over the real socket: a registration that names its sender, one of
    /// its sender's parents or a process its sender started is answered,
    /// the Codex launcher's shape (a `codex` record of the terminal it
    /// started, with its conversation) among them. One that names another
    /// agent's process from inside another agent's session is refused
    /// `identity_mismatch` and takes nothing over, as is one naming a process
    /// nobody holds that is none of those; the person, outside every agent's
    /// session, may name any; and with the agent check off nothing is
    /// refused.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_registration_names_only_a_process_its_sender_may_speak_for() {
        use std::io::{BufRead as _, Write as _};
        let tmp = tempfile::tempdir().unwrap();
        let start = |check: bool| {
            let tmp = tmp.path().join(if check { "on" } else { "off" });
            std::fs::create_dir_all(&tmp).unwrap();
            let mut daemon = Daemon::open(tmp.clone(), tmp.join("host.sock")).unwrap();
            daemon.person_check = false;
            daemon.agent_check = check;
            Arc::new(daemon)
        };
        let daemon = start(true);
        let listener = crate::server::bind(&daemon).await.unwrap();
        let serving = tokio::spawn(crate::server::serve(daemon.clone(), listener));
        let workdir = tmp.path().to_owned();
        let spec = |name: &str, labels: serde_json::Value| {
            serde_json::json!({"name": name, "workdir": workdir, "labels": labels}).to_string()
        };
        // A shell that, once told how, starts the registrar under it.
        let shell = |socket: &std::path::Path| {
            std::process::Command::new("sh")
                .arg("-c")
                .arg("read -r mode spec; python3 -c \"$0\" \"$1\" \"$mode\" \"$spec\"; exit $?")
                .arg(REGISTRAR)
                .arg(socket)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        };
        let send = |mut child: std::process::Child, mode: String, spec: String| async move {
            writeln!(child.stdin.take().unwrap(), "{mode} {spec}").unwrap();
            let output = tokio::task::spawn_blocking(move || child.wait_with_output())
                .await
                .unwrap()
                .unwrap();
            serde_json::from_slice::<Response>(&output.stdout)
                .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stdout)))
        };
        let registered = |reply: &Response| match reply {
            Response::Agent { agent } => agent.clone(),
            other => panic!("{other:?}"),
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

        // The peer itself, its parent, and a process it started.
        let reply = send(shell(&daemon.socket), "self".into(), spec("own", json!({}))).await;
        registered(&reply);
        let parent = shell(&daemon.socket);
        let host = parent.id();
        let reply = send(parent, "parent".into(), spec("host", json!({}))).await;
        assert_eq!(registered(&reply).pid, Some(host), "the hook's provider");
        let reply = send(
            shell(&daemon.socket),
            "child".into(),
            spec("child", json!({})),
        )
        .await;
        registered(&reply);
        // The Codex launcher: a terminal it started, with its conversation.
        let reply = send(
            shell(&daemon.socket),
            "child".into(),
            serde_json::json!({
                "name": "codex-terminal",
                "runtime": "codex",
                "workdir": workdir,
                "labels": {"session_id": THREAD},
            })
            .to_string(),
        )
        .await;
        assert_eq!(registered(&reply).spec.runtime, "codex");

        // X and Y, each a shell of its own.
        let register = |name: &str, pid: u32| {
            let daemon = daemon.clone();
            let name = name.to_owned();
            let workdir = workdir.clone();
            async move {
                match daemon
                    .handle(Request::Register {
                        spec: AgentSpec {
                            name,
                            workdir: Some(workdir),
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
        let mut x_shell = shell(&daemon.socket);
        let x = register("agent-x", x_shell.id()).await;
        let y_shell = shell(&daemon.socket);
        register("agent-y", y_shell.id()).await;
        let agents = lock(&daemon.state).registry.all().count();
        // Y's child names X's process, with a session to bind onto it.
        let reply = send(
            y_shell,
            x_shell.id().to_string(),
            spec("impostor", json!({"session_id": "stolen"})),
        )
        .await;
        assert!(refused(&reply), "{reply:?}");
        assert!(
            matches!(&reply, Response::Error { message, details: Some(details), .. }
                if message.contains("agent-x") && details["agent"] == x.id.as_str()
                    && details["pid"] == x_shell.id()),
            "{reply:?}"
        );
        // Nor a process nobody holds that is not its own to name.
        let mut stranger = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let z_shell = shell(&daemon.socket);
        register("agent-z", z_shell.id()).await;
        let reply = send(
            z_shell,
            stranger.id().to_string(),
            spec("stranger", json!({})),
        )
        .await;
        assert!(refused(&reply), "{reply:?}");
        assert!(
            matches!(&reply, Response::Error { details: Some(details), .. }
                if details["pid"] == stranger.id() && details.get("agent").is_none()),
            "{reply:?}"
        );
        {
            let state = lock(&daemon.state);
            assert_eq!(state.registry.all().count(), agents + 1, "only Z was added");
            let x = state.registry.get(&x.id).unwrap();
            assert!(!x.spec.labels.contains_key("session_id"), "{x:?}");
        }

        // The person, outside every agent's session, names X's process.
        let mut orphan = std::process::Command::new("sh")
            .arg("-c")
            .arg("python3 -c \"$0\" \"$1\" \"$2\" \"$3\" & read line; exit 0")
            .arg(format!(
                "import os,sys,time\nparent=os.getppid()\nprint('ready',flush=True)\nwhile os.getppid()==parent:\n    time.sleep(0.01)\n{REGISTRAR}"
            ))
            .arg(&daemon.socket)
            .arg(x_shell.id().to_string())
            .arg(spec("persons", json!({})))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let reply = tokio::task::spawn_blocking(move || {
            let mut out = std::io::BufReader::new(orphan.stdout.take().unwrap());
            let mut line = String::new();
            out.read_line(&mut line).unwrap();
            assert_eq!(line.trim(), "ready");
            writeln!(orphan.stdin.take().unwrap(), "exit").unwrap();
            orphan.wait().unwrap();
            line.clear();
            out.read_line(&mut line).unwrap();
            serde_json::from_str::<Response>(&line).unwrap()
        })
        .await
        .unwrap();
        assert_eq!(registered(&reply).pid, Some(x_shell.id()), "{reply:?}");
        serving.abort();

        // With the check off, a stranger's process is named from anywhere.
        let daemon = start(false);
        let listener = crate::server::bind(&daemon).await.unwrap();
        let serving = tokio::spawn(crate::server::serve(daemon.clone(), listener));
        let reply = send(
            shell(&daemon.socket),
            stranger.id().to_string(),
            spec("switched-off", json!({})),
        )
        .await;
        registered(&reply);
        let _ = stranger.kill();
        let _ = stranger.wait();
        let _ = x_shell.kill();
        let _ = x_shell.wait();
        serving.abort();
    }

    /// Over the real socket, as the shared-server MCP registers a terminal:
    /// a process under the stand-in for a profile's shared server names a
    /// Codex terminal the test started (no parent of the sender's and no
    /// child of it), whose own arguments resume the conversation the spec
    /// names. Answered only while the stand-in serves the profile's control
    /// socket, and never for a terminal of another conversation.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_shared_codex_server_registers_the_terminal_of_its_conversation() {
        use std::os::unix::process::CommandExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::open(tmp.path().into(), tmp.path().join("host.sock")).unwrap();
        daemon.person_check = false;
        daemon.agent_check = true;
        let daemon = Arc::new(daemon);
        let listener = crate::server::bind(&daemon).await.unwrap();
        let serving = tokio::spawn(crate::server::serve(daemon.clone(), listener));
        let short = tempfile::Builder::new()
            .prefix("ac-")
            .tempdir_in(if cfg!(target_os = "macos") {
                std::path::PathBuf::from("/tmp")
            } else {
                std::env::temp_dir()
            })
            .unwrap();
        let profile = std::fs::canonicalize(short.path()).unwrap().join("p");
        std::fs::create_dir_all(profile.join(codex_control::DIRECTORY)).unwrap();
        let image = profile.join("packages/app-server-daemon/releases/0.162.0/bin/codex");
        std::fs::create_dir_all(image.parent().unwrap()).unwrap();
        std::fs::copy(std::env::current_exe().unwrap(), &image).unwrap();
        // Terminals in the person's own terminal, as far as the sender is
        // concerned: started here, beside the server.
        // A shell keeps the name it is given, where Python's macOS launcher
        // takes its own; it waits on a read of its own, starting nothing.
        let terminal = |thread: &str| {
            std::process::Command::new("/bin/sh")
                .arg0("codex")
                .args(["-c", "read -r line; exit", "resume", thread])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap()
        };
        /// The terminals, ended however the test ends.
        struct Reaped(Vec<std::process::Child>);
        impl Drop for Reaped {
            fn drop(&mut self) {
                for child in &mut self.0 {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        let terminals = Reaped(vec![
            terminal(THREAD),
            terminal("019a0000-0000-7000-8000-000000000002"),
        ]);
        let (ours, other) = (terminals.0[0].id(), terminals.0[1].id());
        let request = |pid: u32| {
            serde_json::json!({
                "op": "register",
                "spec": {
                    "name": format!("codex-{pid}"),
                    "runtime": "codex",
                    "workdir": tmp.path(),
                    "labels": {
                        "session_id": THREAD,
                        (agentdocker_core::agent::PROFILE_LABEL): profile,
                    },
                },
                "pid": pid,
            })
            .to_string()
        };
        let client = "import socket,sys\ns=socket.socket(socket.AF_UNIX)\ns.connect(sys.argv[-2])\ns.sendall(sys.argv[-1].encode()+b'\\n')\nprint(s.makefile().readline(),end='',flush=True)\n";
        let stand_in = |socket: Option<&std::path::Path>, request: String| {
            let told = serde_json::json!({
                "socket": socket,
                "client": client,
                "daemon": daemon.socket,
                "request": request,
            });
            let image = image.clone();
            async move {
                let output = tokio::task::spawn_blocking(move || {
                    std::process::Command::new(&image)
                        .args([
                            "daemon::acting::tests::shared_server_stand_in",
                            "--exact",
                            "--ignored",
                            "--nocapture",
                            "--test-threads=1",
                        ])
                        .env("AGENTDOCKER_TEST_STAND_IN", told.to_string())
                        .output()
                        .unwrap()
                })
                .await
                .unwrap();
                let text = String::from_utf8_lossy(&output.stdout).into_owned();
                let line = text
                    .lines()
                    .find_map(|line| line.split_once("reply:").map(|(_, reply)| reply))
                    .unwrap_or_else(|| panic!("no reply in {text}"))
                    .to_owned();
                serde_json::from_str::<Response>(&line).unwrap()
            }
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
        // Each stand-in binds the socket afresh.
        let socket = codex_control::socket_path(&profile);
        let served = |request: String| {
            let _ = std::fs::remove_file(&socket);
            stand_in(Some(&socket), request)
        };

        let reply = stand_in(None, request(ours)).await;
        assert!(
            refused(&reply),
            "a Codex image that serves nothing: {reply:?}"
        );
        let reply = served(request(other)).await;
        assert!(
            refused(&reply),
            "another conversation's terminal: {reply:?}"
        );
        assert_eq!(lock(&daemon.state).registry.all().count(), 0);
        let reply = served(request(ours)).await;
        assert!(
            matches!(&reply, Response::Agent { agent } if agent.pid == Some(ours)),
            "the profile's server: {reply:?}"
        );
        drop(terminals);
        serving.abort();
    }
}
