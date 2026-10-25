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
//! started (a launcher names its terminal), or, when it is a Codex terminal
//! nobody holds that resumes the conversation the spec names, from under
//! that profile's proven shared server, where a terminal conversation's
//! hooks and MCP servers run. A process a live agent already holds is named
//! only from inside that agent's session, as a request acting as it would
//! be sent: what a Codex terminal's companion launcher started may name the
//! terminal, and a shared server only a terminal whose own record names its
//! profile, never on the profile the registration names. Otherwise only the
//! person may, and the registration is refused `identity_mismatch`. What
//! the check found goes on with the registration and is held to under the
//! lock that commits it, against the agents that hold the process by then
//! and against a keyed Codex conversation its labels would join, which
//! only that profile's proven server or the person may take, and an ended
//! session it would resume, which a sender inside another live agent's
//! session never may.
//! The same switch turns it off.

use super::*;
use agentdocker_core::HUMAN;
use agentdocker_host::codex_control;
use agentdocker_host::peers;
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
    /// The same, whoever's the process is: where `step` cannot read it
    /// (root's `login` above every macOS terminal), the kernel's process
    /// table, as the person check's trace reads it.
    step_any: &'a dyn Fn(u32) -> Option<(u32, DateTime<Utc>)>,
    /// Why a process could not be read.
    unseen: &'a dyn Fn(u32) -> Unseen,
    /// A process's own arguments.
    arguments: &'a dyn Fn(u32) -> Option<Vec<String>>,
    /// The kernel image a process runs.
    executable: &'a dyn Fn(u32) -> Option<std::path::PathBuf>,
    /// A process's birth.
    born: &'a dyn Fn(u32) -> Option<DateTime<Utc>>,
    /// Whether an executable is AgentDocker's own CLI: this daemon's
    /// companion, or a retained version's ([`person::own_cli`]).
    companion: &'a dyn Fn(&std::path::Path) -> bool,
}

fn step_of_any(pid: u32) -> Option<(u32, DateTime<Utc>)> {
    procinfo::parent_and_birth(pid).or_else(|| {
        Some((
            procinfo::inspect(pid)?.ppid,
            procinfo::start_time_of_any(pid)?,
        ))
    })
}

fn executable_of(pid: u32) -> Option<std::path::PathBuf> {
    procinfo::executable_path_of(pid).ok()
}

fn is_companion(executable: &std::path::Path) -> bool {
    person::own_cli(executable)
}

impl Kernel<'static> {
    /// This host's kernel.
    fn host() -> Self {
        Self {
            step: &procinfo::parent_and_birth,
            step_any: &step_of_any,
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
        // (`authorize_registration`).
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
    launches_codex(launcher, arguments, executable, companion).then_some((launcher, launched))
}

/// Whether `pid` runs this daemon's own CLI (`companion` says of an
/// executable whether it is that file) as `agentdocker codex` or
/// `codex-native`: a Codex terminal's companion launcher.
fn launches_codex(
    pid: u32,
    arguments: impl Fn(u32) -> Option<Vec<String>>,
    executable: impl Fn(u32) -> Option<std::path::PathBuf>,
    companion: impl Fn(&std::path::Path) -> bool,
) -> bool {
    arguments(pid).is_some_and(|argv| {
        argv.iter()
            .skip(1)
            .any(|argument| argument == "codex" || argument == "codex-native")
    }) && executable(pid).is_some_and(|exe| companion(&exe))
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
    /// For a Codex agent, the provider profile its record names, as spelled.
    label: Option<String>,
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
        let codex = record.spec.runtime == "codex";
        let label = record
            .spec
            .labels
            .get(agentdocker_core::agent::PROFILE_LABEL)
            .filter(|_| codex)
            .cloned();
        Self {
            id: record.id.clone(),
            name: record.spec.name.clone(),
            codex: codex.then(|| Codex {
                terminal: own,
                profile: label.as_ref().map(std::path::PathBuf::from),
            }),
            label,
            anchors,
        }
    }
}

/// What a registration's spec says of the process it names when that is a
/// Codex terminal whose conversation runs in a profile's shared server: the
/// profile as spelled and canonical, and the conversation's thread.
struct Terminal {
    label: String,
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
        let label = spec.labels.get(agentdocker_core::agent::PROFILE_LABEL)?;
        Some(Self {
            label: label.clone(),
            profile: std::fs::canonicalize(label).ok()?,
            thread: thread.clone(),
        })
    }
}

/// A profile's shared server, still to be proven by its control socket
/// ([`served_by`]): the profile as records and the spec spell it, its
/// canonical directory, and the candidates for its server among the
/// sender's parents.
#[derive(Debug, PartialEq, Eq)]
struct Server {
    label: String,
    profile: std::path::PathBuf,
    candidates: Vec<Process>,
}

/// What the kernel said of a registration's sender and the process it names
/// when the registration was checked: what admits it then ([`Named::admits`])
/// and again under the lock that commits it, against the agents that hold
/// the process by then ([`Authorized::recheck`]), so an agent that comes to
/// hold it in between is never joined on a word given for a process nobody
/// held.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Named {
    /// The process named, by pid and its birth.
    process: Process,
    /// The sender and its parents, nearest first, as far as they were walked.
    chain: Vec<Process>,
    /// The sender and its parents walked as the person check walks them,
    /// past another user's process `chain` cannot read (root's `login` above
    /// a terminal): what the sender is judged to run outside of.
    reach: Vec<Process>,
    /// Whether that walk reached the top: false when it stopped at this
    /// user's process that cannot be read, went deeper than it traces, or
    /// found the sender gone, so what stands above `reach` is not known.
    whole: bool,
    /// Whether the sender stands among the named process's parents: it
    /// started it.
    started: bool,
    /// Codex terminals, by pid and birth, whose companion launcher stands in
    /// `chain`, among those looked at (`evaluated`).
    launched: Vec<Process>,
    /// The same, for `reach`.
    launched_reach: Vec<Process>,
    /// The Codex terminals whose launcher was looked for: the process named
    /// and every live Codex agent's when the registration was checked.
    evaluated: Vec<Process>,
    /// Whether a companion launcher stands in `reach` at all.
    launcher_in_chain: bool,
    /// The profile, as the spec spells it, when the named process is a Codex
    /// terminal whose own arguments resume the conversation the spec names.
    terminal: Option<String>,
    /// Profiles, as spelled, whose shared server stands in `chain` and serves
    /// the profile's control socket.
    served: Vec<String>,
    /// Of `terminal`'s profile, when it is in `served`: whether the named
    /// process holds a Unix-domain connection whose other end that proven
    /// server holds, as the kernel says it ([`peers::connected`]) — the
    /// terminal is that server's client.
    connected: Vec<String>,
    /// The profiles, as spelled, whose shared server was looked for: the
    /// spec's and every live Codex agent's when the registration was checked.
    profiles: Vec<String>,
    /// Whether a Codex image in any profile's package cache stands in
    /// `reach`: a shared server, proven or not.
    server_in_chain: bool,
}

impl Named {
    /// Whether this admits naming the process as an agent's that holds it
    /// (`holder`): from inside that agent's session, as the agent check finds
    /// it — one of its own processes in `chain`, its terminal's companion
    /// launcher there, the shared server of the profile its own record
    /// names, proven, or, for a Codex terminal whose record names no profile,
    /// the proven server the kernel shows it is a client of — and never on
    /// what the registration says.
    fn admits_holder(&self, holder: &Acting) -> bool {
        self.holds(holder, &self.chain, &self.launched)
    }

    /// [`Named::admits_holder`], over the parents `chain` and the terminals
    /// `launched` whose launcher stands among them.
    fn holds(&self, holder: &Acting, chain: &[Process], launched: &[Process]) -> bool {
        holder.anchors.iter().any(|anchor| chain.contains(anchor))
            || holder.codex.as_ref().is_some_and(|codex| {
                codex
                    .terminal
                    .is_some_and(|terminal| launched.contains(&terminal))
            })
            || holder
                .label
                .as_ref()
                .is_some_and(|label| self.served.contains(label))
            || self.client_of_served(holder)
    }

    /// Whether `holder` is a Codex terminal whose record names no profile,
    /// the named process itself, which the kernel shows is a client of the
    /// proven shared server of the profile the spec names: that server runs
    /// its conversations, as it runs those of a record that names it.
    fn client_of_served(&self, holder: &Acting) -> bool {
        holder.label.is_none()
            && holder
                .codex
                .as_ref()
                .is_some_and(|codex| codex.terminal == Some(self.process))
            && self
                .terminal
                .as_ref()
                .is_some_and(|label| self.served.contains(label) && self.connected.contains(label))
    }

    /// Whether this admits naming the process while `holders` hold it. One
    /// that live agents hold may be named only as every one of theirs
    /// ([`Named::admits_holder`]): registering it is acting as them, and it
    /// may join any of them. One nobody holds may be named when it is the
    /// sender or one of its parents (the sender runs inside it: a hook naming
    /// its provider, an MCP server its host), when the sender started it (a
    /// launcher naming its terminal), or when it is a Codex terminal resuming
    /// the conversation the spec names and the spec's profile's shared server
    /// is proven among the sender's parents (where a terminal conversation's
    /// hooks and MCP servers run, beside rather than under it).
    fn admits(&self, holders: &[Acting]) -> bool {
        if holders.is_empty() {
            self.chain.contains(&self.process)
                || self.started
                || self
                    .terminal
                    .as_ref()
                    .is_some_and(|label| self.served.contains(label))
        } else {
            holders.iter().all(|holder| self.admits_holder(holder))
        }
    }

    /// Whether the sender runs inside `agent`'s session, as the agent check
    /// finds it ([`Named::admits_holder`]) over every parent the person
    /// check's walk reads (`reach`), counting it inside, too, where
    /// the check did not look: an agent whose terminal's launcher was not
    /// looked for while a companion launcher stands among the sender's
    /// parents, or whose profile's server was not while a shared server
    /// does.
    fn inside(&self, agent: &Acting) -> bool {
        self.holds(agent, &self.reach, &self.launched_reach)
            || agent
                .codex
                .as_ref()
                .and_then(|codex| codex.terminal)
                .is_some_and(|terminal| {
                    self.launcher_in_chain && !self.evaluated.contains(&terminal)
                })
            || agent
                .label
                .as_ref()
                .is_some_and(|label| self.server_in_chain && !self.profiles.contains(label))
    }

    /// What keeps the sender from resuming the ended records `ended` while
    /// `others`, the other live agents, live: nothing when it may act as
    /// every ended record itself; else parents it could not all read, since
    /// one of them may be another agent's, or the agent whose session it
    /// runs inside. Only positive evidence lets a fold through.
    fn fold_blocker(
        &self,
        ended: &[Acting],
        others: impl IntoIterator<Item = Acting>,
    ) -> Option<Blocker> {
        if ended.iter().all(|record| self.admits_holder(record)) {
            return None;
        }
        if let Some(agent) = others.into_iter().find(|agent| self.inside(agent)) {
            return Some(Blocker::Inside(agent));
        }
        (!self.whole).then_some(Blocker::Unknown)
    }
}

/// Why a registration may not resume an ended session ([`Named::fold_blocker`]).
enum Blocker {
    /// The sender's parents could not all be read.
    Unknown,
    /// It runs inside this live agent's session.
    Inside(Acting),
}

/// What the kernel says of the registration's sender (`sender`) and the
/// process it names (`named`, by pid and its birth now), with the agents that
/// hold it (`holders`, their profiles canonical) and the terminal the spec
/// claims: the facts, and the shared servers their control sockets have to
/// prove before [`Named::served`] counts them.
fn gather(
    sender: Process,
    named: Process,
    live: &[Acting],
    terminal: Option<&Terminal>,
    kernel: &Kernel,
) -> (Named, Vec<Server>) {
    let (pid, born) = sender;
    let (chain, _) = lineage(pid, born, kernel.step, kernel.unseen, |_| false);
    let (reach, end) = lineage(pid, born, kernel.step_any, kernel.unseen, |_| false);
    let (named_pid, named_born) = named;
    let started = lineage(named_pid, named_born, kernel.step, kernel.unseen, |p| {
        p == sender
    })
    .1 == End::Found;
    let mut evaluated = vec![named];
    evaluated.extend(live.iter().filter_map(|h| h.codex.as_ref()?.terminal));
    evaluated.sort();
    evaluated.dedup();
    let launchers: Vec<(Process, Process)> = evaluated
        .iter()
        .filter_map(|terminal| {
            codex_launcher(
                *terminal,
                kernel.step,
                kernel.arguments,
                kernel.executable,
                kernel.companion,
            )
            .map(|launcher| (*terminal, launcher))
        })
        .collect();
    let started_from = |parents: &[Process]| -> Vec<Process> {
        launchers
            .iter()
            .filter(|(_, launcher)| parents.contains(launcher))
            .map(|(terminal, _)| *terminal)
            .collect()
    };
    let launched = started_from(&chain);
    let launched_reach = started_from(&reach);
    let resumes = terminal.filter(|terminal| {
        (kernel.born)(named_pid) == Some(named_born)
            && (kernel.arguments)(named_pid)
                .is_some_and(|argv| procinfo::codex_resumes(&argv, &terminal.thread))
    });
    let mut profiles: Vec<(String, std::path::PathBuf)> = live
        .iter()
        .filter_map(|holder| {
            Some((
                holder.label.clone()?,
                holder.codex.as_ref()?.profile.clone()?,
            ))
        })
        .collect();
    // The spec's own profile too, whether or not the process resumes its
    // thread: a keyed record of that profile the registration would join is
    // the profile's shared server's to name ([`Authorized::join_refusal`]).
    profiles.extend(terminal.map(|t| (t.label.clone(), t.profile.clone())));
    profiles.sort();
    profiles.dedup();
    let mut looked: Vec<String> = live.iter().filter_map(|h| h.label.clone()).collect();
    looked.extend(terminal.map(|t| t.label.clone()));
    let launcher_in_chain = reach.iter().any(|(pid, _)| {
        launches_codex(*pid, kernel.arguments, kernel.executable, kernel.companion)
    });
    let server_in_chain = reach.iter().any(|(pid, _)| {
        (kernel.executable)(*pid)
            .and_then(|image| std::fs::canonicalize(image).ok())
            .is_some_and(|image| codex_control::shared_server_profile(&image).is_some())
    });
    let servers = profiles
        .into_iter()
        .filter_map(|(label, profile)| {
            let candidates =
                shared_server_candidates(&chain, &profile, kernel.executable, kernel.born);
            (!candidates.is_empty()).then_some(Server {
                label,
                profile,
                candidates,
            })
        })
        .collect();
    (
        Named {
            process: named,
            chain,
            reach,
            whole: end != End::Unknown,
            started,
            launched,
            launched_reach,
            evaluated,
            launcher_in_chain,
            terminal: resumes.map(|t| t.label.clone()),
            served: Vec::new(),
            connected: Vec::new(),
            profiles: looked,
            server_in_chain,
        },
        servers,
    )
}

/// What a registration's check let its `pid` through on, for the
/// registration to hold to when it commits.
#[derive(Clone, Debug)]
pub(crate) enum Authorized {
    /// The person's, who may register any process: nothing to hold to.
    Person,
    /// What the sender may name, as the kernel said it.
    Named(Box<Named>),
}

impl Authorized {
    /// Under the lock that commits the registration, which recorded the
    /// named process's birth as `born`: a refusal, with nothing done, when
    /// the process is not the one checked any more, or what was checked no
    /// longer admits it against the agents that hold it now — one that came
    /// to hold it after a check that found nobody, say. `None` lets it
    /// through.
    pub(super) fn recheck(&self, state: &State, born: Option<DateTime<Utc>>) -> Option<Response> {
        let Authorized::Named(named) = self else {
            return None;
        };
        let (pid, checked) = named.process;
        if born != Some(checked) {
            return Some(naming_refusal(
                pid,
                None,
                "another process has that pid since it was checked",
            ));
        }
        let holders: Vec<Acting> = state
            .registry
            .live()
            .map(Acting::of)
            .filter(|holder| holder.anchors.contains(&named.process))
            .collect();
        if named.admits(&holders) {
            return None;
        }
        let holder = holders
            .iter()
            .find(|holder| !named.admits_holder(holder))
            .or(holders.first());
        Some(naming_refusal(
            pid,
            holder,
            "it came to be held after it was checked, by an agent whose session it was not sent from",
        ))
    }

    /// Under the lock that commits the registration, which would join an
    /// existing record of the conversation its labels name (`target`, a
    /// Codex conversation known by its key alone, with no process): a
    /// refusal, with nothing done, unless the sender may act as that record
    /// as the agent check finds it — for a keyed record, from under the
    /// proven shared server of the profile it names — or the person sent
    /// it. The registration's own labels select the record and prove
    /// nothing about who may take it. `None` lets it through.
    pub(super) fn join_refusal(&self, target: &AgentRecord) -> Option<Response> {
        let Authorized::Named(named) = self else {
            return None;
        };
        let target = Acting::of(target);
        if named.admits_holder(&target) {
            return None;
        }
        Some(naming_refusal(
            named.process.0,
            Some(&target),
            "it would join that agent's conversation, whose session it was not sent from",
        ))
    }

    /// Under the lock that commits the registration, which would resume the
    /// ended records `ended` (the session its `session_id` names) into the
    /// registration's own record: a refusal, with nothing done, when the
    /// sender runs inside another live agent's session ([`Named::inside`]). A sender inside agent A's
    /// session acts as A, and must never become ended agent B. Let through
    /// from outside every other agent's session (the person's terminal, where
    /// a resumed session's own hooks run under its new process), when the
    /// sender may act as every ended record as the agent check finds it (the
    /// proven shared server of the profile they name), or from the person.
    /// A live record it joins counts as another agent's too: neither its
    /// labels, the registration's, nor its process's command line, which
    /// whoever runs in it can choose, are evidence that it may become the
    /// ended one.
    pub(super) fn fold_refusal(&self, state: &State, ended: &[AgentRecord]) -> Option<Response> {
        let Authorized::Named(named) = self else {
            return None;
        };
        let prior = ended.first()?;
        let ended: Vec<Acting> = ended.iter().map(Acting::of).collect();
        let others = state.registry.live().map(Acting::of);
        let blocker = named.fold_blocker(&ended, others)?;
        let pid = named.process.0;
        let (why, inside) = match &blocker {
            Blocker::Unknown => (
                "its sender's parents could not all be read, so it cannot be told to run \
                 outside every other agent's session"
                    .to_owned(),
                None,
            ),
            Blocker::Inside(agent) => (
                format!("it was sent from inside agent {}'s session", agent.name),
                Some(agent.id.clone()),
            ),
        };
        Some(Response::Error {
            code: ErrorCode::IdentityMismatch,
            message: format!(
                "this registers process {pid} to resume agent {}'s ended session, and {why}; an \
                 agent's process acts only as that agent, so a session is resumed from outside \
                 every other agent's session, or by the person",
                prior.spec.name
            ),
            details: Some(json!({
                "pid": pid,
                "agent": prior.id,
                "name": prior.spec.name,
                "inside": inside,
            })),
        })
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

    /// What lets `request`, a registration of a process (`register`'s or
    /// `adopt`'s `pid`), through from `peer`, the process on the socket: what the
    /// kernel says the sender may name ([`Named::admits`]), or the person,
    /// who may register any process. `Ok(None)` for a request that names no
    /// process, with the agent check off, or from the daemon's own process,
    /// which are not checked, as [`Daemon::agent_refusal`] does not check
    /// them; `Err` with the refusal otherwise. What was admitted goes on to
    /// the registration, which holds to it when it commits
    /// ([`Authorized::recheck`]).
    pub(crate) async fn authorize_registration(
        self: &Arc<Self>,
        peer: Option<Peer>,
        request: &Request,
    ) -> Result<Option<Authorized>, Box<Response>> {
        // An adoption registers the process it names as a registration
        // does, with a spec of the daemon's own making that names no
        // session.
        let (pid, spec) = match request {
            Request::Register {
                spec,
                pid: Some(pid),
                ..
            } => (*pid, spec.clone()),
            Request::Adopt { pid, .. } => (*pid, AgentSpec::default()),
            _ => return Ok(None),
        };
        if !self.agent_check || peer.is_some_and(|(sender, _)| sender == std::process::id()) {
            return Ok(None);
        }
        let live: Vec<Acting> = lock(&self.state).registry.live().map(Acting::of).collect();
        let claimed = keyed_claim(&spec).cloned();
        let identified = peer.and_then(|(sender, born)| born.map(|born| (sender, born)));
        let (found, holders) = tokio::task::spawn_blocking(move || {
            let named = procinfo::start_time(pid).map(|born| (pid, born));
            let live: Vec<Acting> = live
                .into_iter()
                .map(|mut agent| {
                    if let Some(codex) = &mut agent.codex {
                        codex.profile = codex
                            .profile
                            .take()
                            .map(|p| std::fs::canonicalize(&p).unwrap_or(p));
                    }
                    agent
                })
                .collect();
            let holders: Vec<Acting> = named
                .map(|named| {
                    live.iter()
                        .filter(|holder| holder.anchors.contains(&named))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            let found = identified.zip(named).map(|(sender, named)| {
                gather(
                    sender,
                    named,
                    &live,
                    Terminal::of(&spec).as_ref(),
                    &Kernel::host(),
                )
            });
            (found, holders)
        })
        .await
        .unwrap_or((None, Vec::new()));
        let mut admitted = None;
        if let Some((mut named, servers)) = found {
            for server in servers {
                let serving = tokio::time::timeout(
                    SERVER_CHECK,
                    codex_control::serving_process(&server.profile),
                )
                .await
                .ok()
                .and_then(Result::ok)
                .flatten();
                if served_by(&server.candidates, serving, procinfo::start_time) {
                    if let Some(serving) = serving
                        && named.terminal.as_ref() == Some(&server.label)
                    {
                        let terminal = named.process.0;
                        let client = tokio::task::spawn_blocking(move || {
                            peers::connected(terminal, serving)
                        })
                        .await
                        .ok()
                        .flatten();
                        if client == Some(true) {
                            named.connected.push(server.label.clone());
                        }
                    }
                    named.served.push(server.label);
                }
            }
            if named.admits(&holders) {
                // A spec that names a Codex conversation's profile and
                // thread may join that conversation's keyed record, which
                // only that profile's server or the person may take
                // ([`Authorized::join_refusal`]): unless the server is
                // proven, whether the person sent it is asked as well.
                let joins = claimed
                    .as_ref()
                    .is_some_and(|label| !named.served.contains(label));
                if !joins {
                    return Ok(Some(Authorized::Named(Box::new(named))));
                }
                admitted = Some(named);
            }
        }
        let why = match self.trace(peer, request).await {
            person::Traced::Person => return Ok(Some(Authorized::Person)),
            _ if admitted.is_some() => {
                return Ok(admitted.map(|named| Authorized::Named(Box::new(named))));
            }
            person::Traced::Inside(reason) => format!("it was sent from inside {reason}"),
            person::Traced::Unknown(reason) => format!("{reason} cannot be the person's"),
        };
        let sender = peer.map_or(0, |(sender, _)| sender);
        warn!(peer = sender, pid, %why, "refused a registration of a process its sender may not name");
        Err(Box::new(naming_refusal(pid, holders.first(), &why)))
    }
}

/// The profile, as spelled, of the Codex conversation a registration's spec
/// names by its profile and thread: the key a keyed record of that
/// conversation is found by when the registration commits.
fn keyed_claim(spec: &AgentSpec) -> Option<&String> {
    (spec.runtime == "codex"
        && spec
            .labels
            .get("session_id")
            .is_some_and(|thread| !thread.is_empty()))
    .then(|| spec.labels.get(agentdocker_core::agent::PROFILE_LABEL))
    .flatten()
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
            format!("this registers process {pid}, and {why}; {RULE}"),
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
            priority: 0,
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
        /// This user's processes that run and cannot be read.
        unreadable: Vec<u32>,
        /// Parents only the kernel's process table gives (another user's
        /// process, as root's `login` is).
        kernel_only: HashMap<u32, u32>,
    }

    impl Table {
        /// What the kernel says `sender` may name of `named` while
        /// `holders` hold it and the spec claims `terminal`, with `serving`
        /// the process every profile's control socket names.
        fn named(
            &self,
            sender: u32,
            named: (u32, DateTime<Utc>),
            holders: &[Acting],
            terminal: Option<&Terminal>,
            serving: Option<u32>,
        ) -> Named {
            let step = |pid: u32| {
                self.parents
                    .get(&pid)
                    .map(|parent| (*parent, at(i64::from(pid))))
            };
            let born = |pid: u32| self.parents.contains_key(&pid).then(|| at(i64::from(pid)));
            let arguments = |pid: u32| self.argv.get(&pid).cloned();
            let executable = |pid: u32| self.images.get(&pid).cloned();
            let companion = |exe: &std::path::Path| exe == std::path::Path::new("/opt/agentdocker");
            let unseen = |pid: u32| {
                if self.unreadable.contains(&pid) {
                    Unseen::Unreadable
                } else {
                    Unseen::Ended
                }
            };
            let step_any = |pid: u32| {
                step(pid).or_else(|| {
                    self.kernel_only
                        .get(&pid)
                        .map(|parent| (*parent, at(i64::from(pid))))
                })
            };
            let kernel = Kernel {
                step: &step,
                step_any: &step_any,
                unseen: &unseen,
                arguments: &arguments,
                executable: &executable,
                born: &born,
                companion: &companion,
            };
            let (mut facts, servers) = gather(
                (sender, at(i64::from(sender))),
                named,
                holders,
                terminal,
                &kernel,
            );
            for server in servers {
                if served_by(&server.candidates, serving, born) {
                    facts.served.push(server.label);
                }
            }
            facts
        }

        /// Whether `sender` may name `named` while `holders` hold it, the
        /// spec claiming `terminal`, every control socket served by 60:
        /// profile P's shared server.
        fn admits(
            &self,
            sender: u32,
            named: u32,
            holders: &[Acting],
            terminal: Option<&Terminal>,
        ) -> bool {
            self.named(
                sender,
                (named, at(i64::from(named))),
                holders,
                terminal,
                Some(60),
            )
            .admits(holders)
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
    /// [`THREAD`] (81), and another conversation (82). Above 91, and above P's
    /// server's copy 93 with its child 94, stands this user's process 90,
    /// which cannot be read; above 97, another user's 96, which only the
    /// kernel's table reads.
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
            (91, 90),
            (93, 90),
            (94, 93),
            (97, 96),
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
            (60, p_image.clone()),
            (70, q_image),
            (93, p_image),
        ]);
        (
            Table {
                parents,
                argv,
                images,
                unreadable: vec![90, 96],
                kernel_only: HashMap::from([(96, 1)]),
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
        assert!(table.admits(21, 21, &[], None));
        // One of its parents: an MCP server its host, a hook its provider
        // past the shell between them.
        assert!(table.admits(21, 20, &[], None));
        assert!(table.admits(23, 20, &[], None));
        // A process it started, directly or below a child.
        assert!(table.admits(50, 51, &[], None));
        assert!(table.admits(20, 23, &[], None));
        // The Codex launcher names the terminal it started.
        assert!(table.admits(30, 31, &[], None));
        // A sibling, a cousin or a stranger is not.
        assert!(!table.admits(21, 22, &[], None));
        assert!(!table.admits(41, 51, &[], None));
        assert!(!table.admits(41, 20, &[], None));
        // A process given the named pid since is not the one walked.
        assert!(!table.named(21, (20, at(19)), &[], None, None).admits(&[]));
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
        assert!(table.admits(23, 20, &x, None), "X's hook");
        assert!(table.admits(20, 20, &x, None), "X itself");
        // A process a recorded agent's launcher started: the Codex
        // terminal Y, re-registered from under the server its launcher
        // started beside it.
        let y = [holder("codex", 31, None)];
        assert!(table.admits(33, 31, &y, None));
        assert!(table.admits(30, 31, &y, None));
        // Impostors.
        assert!(!table.admits(41, 20, &x, None));
        assert!(!table.admits(40, 20, &x, None));
        assert!(!table.admits(41, 31, &y, None));
        assert!(!table.admits(21, 31, &y, None));
        // Z started W, and still may not take W's identity.
        let w = [holder("claude-code", 42, None)];
        assert!(table.admits(41, 42, &[], None));
        assert!(!table.admits(41, 42, &w, None));
        assert!(!table.admits(10, 42, &w, None));
    }

    /// From under a profile's shared server, a Codex terminal nobody holds
    /// whose own arguments resume the conversation the spec names may be
    /// named as a record of that profile, when the server among the
    /// sender's parents is a Codex image in that profile's package cache,
    /// and then only if it serves the profile's control socket
    /// ([`served_by`]). A terminal a live agent holds is named from
    /// there only when that agent's own record names the same profile, as
    /// the agent check allows: never on the profile the registration names,
    /// so a record that names none, another profile's or another runtime's
    /// is not taken over. Not a terminal of another conversation, not from
    /// another profile's server, and not from an ordinary process.
    #[test]
    fn a_shared_server_names_the_terminal_that_resumes_its_conversation() {
        let tmp = tempfile::tempdir().unwrap();
        let (table, p, q) = table(&std::fs::canonicalize(tmp.path()).unwrap());
        let terminal = |profile: &std::path::Path, thread: &str| Terminal {
            label: profile.display().to_string(),
            profile: profile.to_owned(),
            thread: thread.to_owned(),
        };
        let theirs = terminal(&p, THREAD);
        assert!(table.admits(61, 81, &[], Some(&theirs)));
        // Only while the server serves the profile's control socket.
        let at81 = (81, at(81));
        for serving in [None, Some(61)] {
            assert!(
                !table
                    .named(61, at81, &[], Some(&theirs), serving)
                    .admits(&[])
            );
        }
        // Held by a Codex record of the same profile: its own record proves
        // it, whatever the spec says.
        let ours = [holder("codex", 81, Some(&p))];
        assert!(table.admits(61, 81, &ours, Some(&theirs)));
        assert!(table.admits(61, 81, &ours, None));
        // Another conversation's terminal, or a spec naming another thread.
        assert!(!table.admits(61, 82, &[], Some(&theirs)));
        // From another profile's server, or naming this terminal for it.
        assert!(!table.admits(71, 81, &[], Some(&theirs)));
        assert!(!table.admits(61, 81, &[], Some(&terminal(&q, THREAD))));
        // Held by a Codex record that names no profile, another profile's
        // agent, or another runtime's: the spec's profile proves nothing.
        for held in [
            holder("codex", 81, None),
            holder("codex", 81, Some(&q)),
            holder("claude-code", 81, None),
        ] {
            assert!(!table.admits(61, 81, &[held], Some(&theirs)));
        }
        // From an ordinary process, or without a terminal's claim.
        assert!(!table.admits(41, 81, &[], Some(&theirs)));
        assert!(!table.admits(61, 81, &[], None));
    }

    /// A Codex terminal whose record names no profile (adopted, or made
    /// first by an MCP server) is named from under a profile's proven shared
    /// server when the kernel shows the terminal is that server's client,
    /// and a sender there then counts as inside its session. Not on the
    /// connection without the server proven, not for a spec naming another
    /// conversation, and never for a record of another profile or of
    /// another runtime.
    #[test]
    fn a_terminal_that_names_no_profile_is_named_as_its_server_s_client() {
        let tmp = tempfile::tempdir().unwrap();
        let (table, p, q) = table(&std::fs::canonicalize(tmp.path()).unwrap());
        let terminal = |thread: &str| Terminal {
            label: p.display().to_string(),
            profile: p.clone(),
            thread: thread.to_owned(),
        };
        let theirs = terminal(THREAD);
        let at81 = (81, at(81));
        let unprofiled = holder("codex", 81, None);
        let named = |holders: &[Acting], spec: &Terminal, serving, client: bool| {
            let mut named = table.named(61, at81, holders, Some(spec), serving);
            if client {
                named.connected.push(spec.label.clone());
            }
            named
        };
        let held = std::slice::from_ref(&unprofiled);
        assert!(
            !named(held, &theirs, Some(60), false).admits(held),
            "no connection"
        );
        let client = named(held, &theirs, Some(60), true);
        assert!(client.admits(held), "its server's client");
        assert!(client.inside(&unprofiled), "and inside its session");
        assert!(
            !named(held, &theirs, None, true).admits(held),
            "an unproven server"
        );
        let other = terminal("019a0000-0000-7000-8000-000000000002");
        assert!(
            !named(held, &other, Some(60), true).admits(held),
            "another conversation's spec"
        );
        for record in [
            holder("codex", 81, Some(&q)),
            holder("claude-code", 81, None),
        ] {
            let held = std::slice::from_ref(&record);
            assert!(
                !named(held, &theirs, Some(60), true).admits(held),
                "{}",
                record.name
            );
        }
    }

    /// What a check found holds when the registration commits only against
    /// the agents that hold the process by then ([`Authorized::recheck`]):
    /// a hook's word for its provider holds whoever came to hold it (each
    /// holds it as its own, and the hook runs under it), the Codex
    /// launcher's for its terminal holds against the terminal's own record
    /// and not another runtime's, and a shared server's for a terminal
    /// nobody held holds against a record that names its profile, never one
    /// that names none. Every holder must be admitted, not one of them.
    #[test]
    fn a_check_holds_only_against_the_agents_that_hold_the_process_at_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let (table, p, _) = table(&std::fs::canonicalize(tmp.path()).unwrap());
        let hook = table.named(23, (20, at(20)), &[], None, None);
        assert!(hook.admits(&[]));
        assert!(hook.admits(&[holder("claude-code", 20, None)]));
        let launcher = table.named(30, (31, at(31)), &[], None, None);
        assert!(launcher.admits(&[]));
        assert!(launcher.admits(&[holder("codex", 31, None)]));
        assert!(!launcher.admits(&[holder("claude-code", 31, None)]));
        assert!(!launcher.admits(&[holder("codex", 31, None), holder("claude-code", 31, None)]));
        let theirs = Terminal {
            label: p.display().to_string(),
            profile: p.clone(),
            thread: THREAD.to_owned(),
        };
        let shared = table.named(61, (81, at(81)), &[], Some(&theirs), Some(60));
        assert!(shared.admits(&[]));
        assert!(!shared.admits(&[holder("codex", 81, None)]));
        assert!(shared.admits(&[holder("codex", 81, Some(&p))]));
    }

    /// Whether a sender runs inside another live agent's session, which
    /// keeps it from resuming an ended one ([`Authorized::fold_refusal`]):
    /// as the agent check finds it for the agents looked at, and inside,
    /// too, for one the check did not look at while a companion launcher or
    /// a shared server stands among the sender's parents.
    #[test]
    fn a_sender_inside_an_agent_s_session_is_found_there_or_assumed_there() {
        let tmp = tempfile::tempdir().unwrap();
        let (table, p, q) = table(&std::fs::canonicalize(tmp.path()).unwrap());
        let x = holder("claude-code", 20, None);
        let y = holder("codex", 31, None);
        // Under X's own process; under Y's launcher, looked at or not.
        assert!(table.named(23, (51, at(51)), &[], None, None).inside(&x));
        assert!(
            table
                .named(33, (51, at(51)), std::slice::from_ref(&y), None, None)
                .inside(&y)
        );
        assert!(table.named(33, (51, at(51)), &[], None, None).inside(&y));
        // Nothing of either among a stranger's parents.
        let stranger = table.named(41, (51, at(51)), &[], None, None);
        assert!(!stranger.inside(&x) && !stranger.inside(&y));
        // Under P's proven server: inside P's agents, not Q's, which it
        // looked at; inside an agent of a profile it did not look at.
        let ours = holder("codex", 81, Some(&p));
        let theirs = holder("codex", 82, Some(&q));
        let live = [ours.clone(), theirs.clone()];
        let server = table.named(61, (51, at(51)), &live, None, Some(60));
        assert!(server.inside(&ours) && !server.inside(&theirs));
        assert!(
            table
                .named(61, (51, at(51)), &[], None, Some(60))
                .inside(&theirs)
        );
    }

    /// A sender whose parents could not all be read may still name itself,
    /// but cannot show that it runs outside every other agent's session, so
    /// it resumes no ended session ([`Named::fold_blocker`]) unless it may
    /// act as the ended record itself: from under the proven shared server
    /// of the profile that record names. One whose parents reach the top
    /// resumes it, unless one of them is a live agent's.
    #[test]
    fn a_sender_whose_parents_cannot_all_be_read_resumes_no_ended_session() {
        let tmp = tempfile::tempdir().unwrap();
        let (table, p, _) = table(&std::fs::canonicalize(tmp.path()).unwrap());
        let ended = [holder("claude-code", 99, None)];
        let hidden = table.named(91, (91, at(91)), &[], None, None);
        assert!(!hidden.whole);
        assert!(hidden.admits(&[]), "it may name itself");
        assert!(matches!(
            hidden.fold_blocker(&ended, []),
            Some(Blocker::Unknown)
        ));
        // Past another user's process the kernel's table reads, the walk
        // reaches the top.
        let terminal = table.named(97, (97, at(97)), &[], None, None);
        assert!(terminal.whole);
        assert!(terminal.fold_blocker(&ended, []).is_none());
        let open = table.named(51, (51, at(51)), &[], None, None);
        assert!(open.whole);
        assert!(open.fold_blocker(&ended, []).is_none());
        assert!(matches!(
            open.fold_blocker(&ended, [holder("claude-code", 50, None)]),
            Some(Blocker::Inside(_))
        ));
        let theirs = Terminal {
            label: p.display().to_string(),
            profile: p.clone(),
            thread: THREAD.to_owned(),
        };
        let server = table.named(94, (94, at(94)), &[], Some(&theirs), Some(93));
        assert!(!server.whole);
        assert!(
            server
                .fold_blocker(&[holder("codex", 98, Some(&p))], [])
                .is_none(),
            "its profile's proven server"
        );
        assert!(matches!(
            server.fold_blocker(&ended, []),
            Some(Blocker::Unknown)
        ));
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

    /// A daemon with the agent check on, beside the stand-in for a Codex
    /// profile's shared server (this test binary copied into the profile's
    /// package cache) and the Codex terminals a test starts: in the person's
    /// own terminal as far as the stand-in is concerned, neither its parents
    /// nor its children.
    #[cfg(unix)]
    struct SharedServer {
        daemon: Arc<Daemon>,
        serving: tokio::task::JoinHandle<anyhow::Result<()>>,
        workdir: std::path::PathBuf,
        profile: std::path::PathBuf,
        image: std::path::PathBuf,
        terminals: Vec<std::process::Child>,
        /// The other end of a terminal's connection, for the next stand-in
        /// to hold.
        peer: std::sync::Mutex<Option<std::os::fd::OwnedFd>>,
        /// Other ends this test's own process holds.
        held: Vec<std::os::fd::OwnedFd>,
        _dirs: (tempfile::TempDir, tempfile::TempDir),
    }

    #[cfg(unix)]
    impl SharedServer {
        async fn start() -> Self {
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
            Self {
                daemon,
                serving,
                workdir: tmp.path().to_owned(),
                profile,
                image,
                terminals: Vec::new(),
                peer: std::sync::Mutex::new(None),
                held: Vec::new(),
                _dirs: (tmp, short),
            }
        }

        /// A Codex terminal resuming `thread`, as [`SharedServer::terminal`],
        /// that holds one end of a Unix-domain connection: the next stand-in
        /// holds the other end when `to_server` says so, as a terminal's own
        /// connection to its server is held, and this test's process
        /// otherwise, as another server would.
        fn connected_terminal(&mut self, thread: &str, to_server: bool) -> u32 {
            use std::os::fd::AsRawFd as _;
            use std::os::unix::process::CommandExt as _;
            let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
            let raw = theirs.as_raw_fd();
            let mut command = std::process::Command::new("/bin/sh");
            command
                .arg0("codex")
                .args(["-c", "read -r line; exit", "resume", thread])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            // SAFETY: dup2 and fcntl are async-signal-safe, and the closure
            // touches nothing else between fork and exec.
            unsafe {
                command.pre_exec(move || {
                    if raw == 3 {
                        let flags = libc::fcntl(3, libc::F_GETFD);
                        libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                    } else if libc::dup2(raw, 3) != 3 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = command.spawn().unwrap();
            drop(theirs);
            let pid = child.id();
            self.terminals.push(child);
            let ours = std::os::fd::OwnedFd::from(ours);
            if to_server {
                *self.peer.lock().unwrap() = Some(ours);
            } else {
                self.held.push(ours);
            }
            pid
        }

        /// A Codex terminal resuming `thread`, by its pid. A shell keeps
        /// the name it is given, where Python's macOS launcher takes its
        /// own; it waits on a read of its own, starting nothing.
        fn terminal(&mut self, thread: &str) -> u32 {
            use std::os::unix::process::CommandExt as _;
            let child = std::process::Command::new("/bin/sh")
                .arg0("codex")
                .args(["-c", "read -r line; exit", "resume", thread])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let pid = child.id();
            self.terminals.push(child);
            pid
        }

        /// The shared server's registration of `pid` as the terminal of
        /// `thread`, as `native_identity::bind_terminal` sends it.
        fn register(&self, pid: u32, thread: &str) -> String {
            serde_json::json!({
                "op": "register",
                "spec": {
                    "name": format!("codex-{pid}"),
                    "runtime": "codex",
                    "workdir": self.workdir,
                    "labels": {
                        "session_id": thread,
                        (agentdocker_core::agent::PROFILE_LABEL): self.profile,
                    },
                },
                "pid": pid,
            })
            .to_string()
        }

        /// `request`, sent by a process under the stand-in, serving the
        /// profile's control socket (bound afresh) when `serve` says so.
        async fn send(&self, serve: bool, request: String) -> Response {
            let socket = codex_control::socket_path(&self.profile);
            let _ = std::fs::remove_file(&socket);
            let told = serde_json::json!({
                "socket": serve.then_some(socket),
                "client": "import socket,sys\ns=socket.socket(socket.AF_UNIX)\ns.connect(sys.argv[-2])\ns.sendall(sys.argv[-1].encode()+b'\\n')\nprint(s.makefile().readline(),end='',flush=True)\n",
                "daemon": self.daemon.socket,
                "request": request,
            });
            let image = self.image.clone();
            let peer = self.peer.lock().unwrap().take();
            let output = tokio::task::spawn_blocking(move || {
                let mut command = std::process::Command::new(&image);
                command
                    .args([
                        "daemon::acting::tests::shared_server_stand_in",
                        "--exact",
                        "--ignored",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env("AGENTDOCKER_TEST_STAND_IN", told.to_string());
                // The stand-in holds a terminal's connection as its input.
                if let Some(peer) = peer {
                    command.stdin(std::process::Stdio::from(peer));
                }
                command.output().unwrap()
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

        /// A live `codex` record of `pid`, registered by the daemon's own
        /// hand with `labels`.
        async fn record(
            &self,
            pid: u32,
            labels: std::collections::BTreeMap<String, String>,
        ) -> AgentRecord {
            match self
                .daemon
                .handle(Request::Register {
                    spec: AgentSpec {
                        name: format!("terminal-{pid}"),
                        runtime: "codex".into(),
                        workdir: Some(self.workdir.clone()),
                        labels,
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
    }

    #[cfg(unix)]
    impl Drop for SharedServer {
        fn drop(&mut self) {
            for child in &mut self.terminals {
                let _ = child.kill();
                let _ = child.wait();
            }
            self.serving.abort();
        }
    }

    #[cfg(unix)]
    fn identity_mismatch(reply: &Response) -> bool {
        matches!(
            reply,
            Response::Error {
                code: ErrorCode::IdentityMismatch,
                ..
            }
        )
    }

    /// Over the real socket, as the shared-server MCP registers a terminal:
    /// a process under the stand-in names a Codex terminal nobody holds,
    /// whose own arguments resume the conversation the spec names. Answered
    /// only while the stand-in serves the profile's control socket, and
    /// never for a terminal of another conversation.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_shared_codex_server_registers_the_terminal_of_its_conversation() {
        let mut server = SharedServer::start().await;
        let ours = server.terminal(THREAD);
        let other = server.terminal("019a0000-0000-7000-8000-000000000002");

        let reply = server.send(false, server.register(ours, THREAD)).await;
        assert!(
            identity_mismatch(&reply),
            "a Codex image that serves nothing: {reply:?}"
        );
        let reply = server.send(true, server.register(other, THREAD)).await;
        assert!(
            identity_mismatch(&reply),
            "another conversation's terminal: {reply:?}"
        );
        assert_eq!(lock(&server.daemon.state).registry.all().count(), 0);
        let reply = server.send(true, server.register(ours, THREAD)).await;
        assert!(
            matches!(&reply, Response::Agent { agent } if agent.pid == Some(ours)),
            "the profile's server: {reply:?}"
        );
    }

    /// Over the real socket: a Codex terminal whose live record names no
    /// profile (an adopted or MCP-first record) and that is not the shared
    /// server's client is not the server's to name, even when its own
    /// arguments resume the conversation the registration names and the
    /// server is proven: the registration's own profile and thread prove
    /// nothing about the record. It is refused
    /// `identity_mismatch`, the record learns neither a session nor a
    /// profile, and the server still cannot act as it. A terminal whose
    /// own record names the profile is the server's to name, and learns its
    /// conversation.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_shared_server_does_not_take_over_a_record_that_names_no_profile() {
        let mut server = SharedServer::start().await;
        let b = server.terminal(THREAD);
        let unprofiled = server
            .record(b, [("via".to_owned(), "mcp".to_owned())].into())
            .await;
        let before = lock(&server.daemon.state).registry.all().count();

        let reply = server.send(true, server.register(b, THREAD)).await;
        assert!(identity_mismatch(&reply), "{reply:?}");
        assert!(
            matches!(&reply, Response::Error { details: Some(details), .. }
                if details["agent"] == unprofiled.id.as_str() && details["pid"] == b),
            "{reply:?}"
        );
        {
            let state = lock(&server.daemon.state);
            assert_eq!(state.registry.all().count(), before, "nothing added");
            let record = state.registry.get(&unprofiled.id).unwrap();
            assert_eq!(record.spec.labels, unprofiled.spec.labels, "{record:?}");
        }
        // Nor may the server act as it.
        let claim = serde_json::json!({
            "op": "claim",
            "agent": unprofiled.id,
            "resource": format!("path:{}/b", server.workdir.display()),
            "ttl_secs": 60,
        })
        .to_string();
        let reply = server.send(true, claim).await;
        assert!(identity_mismatch(&reply), "{reply:?}");
        assert!(lock(&server.daemon.state).leases.is_empty());

        // A terminal whose own record names this profile is the server's.
        let thread = "019a0000-0000-7000-8000-000000000003";
        let c = server.terminal(thread);
        let profiled = server
            .record(
                c,
                [(
                    agentdocker_core::agent::PROFILE_LABEL.to_owned(),
                    server.profile.display().to_string(),
                )]
                .into(),
            )
            .await;
        let reply = server.send(true, server.register(c, thread)).await;
        assert!(
            matches!(&reply, Response::Agent { agent }
                if agent.id == profiled.id
                    && agent.spec.labels.get("session_id").map(String::as_str) == Some(thread)),
            "{reply:?}"
        );
    }

    /// Over the real socket: a Codex terminal whose live record names no
    /// profile, holding a Unix-domain connection whose other end the
    /// profile's proven shared server holds, is that server's client, as the
    /// kernel says it, and the server's registration binds its record to the
    /// conversation: the same id, learning the thread and the profile. A
    /// terminal connected to another process instead, as one of another
    /// profile's server is, is refused, and its record stays as it was.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_shared_server_binds_a_record_that_names_no_profile_to_its_client() {
        let mut server = SharedServer::start().await;
        let elsewhere = server.connected_terminal(THREAD, false);
        let theirs = server
            .record(elsewhere, [("via".to_owned(), "mcp".to_owned())].into())
            .await;
        let reply = server.send(true, server.register(elsewhere, THREAD)).await;
        assert!(
            matches!(&reply, Response::Error { code: ErrorCode::IdentityMismatch, details: Some(details), .. }
                if details["agent"] == theirs.id.as_str()),
            "another process's client: {reply:?}"
        );
        assert_eq!(
            lock(&server.daemon.state)
                .registry
                .get(&theirs.id)
                .unwrap()
                .spec
                .labels,
            theirs.spec.labels
        );

        let thread = "019a0000-0000-7000-8000-000000000003";
        let client = server.connected_terminal(thread, true);
        let unprofiled = server
            .record(client, [("via".to_owned(), "mcp".to_owned())].into())
            .await;
        let reply = server.send(true, server.register(client, thread)).await;
        let profile = server.profile.display().to_string();
        assert!(
            matches!(&reply, Response::Agent { agent }
                if agent.id == unprofiled.id
                    && agent.pid == Some(client)
                    && agent.spec.labels.get("session_id").map(String::as_str) == Some(thread)
                    && agent.spec.labels.get(agentdocker_core::agent::PROFILE_LABEL)
                        == Some(&profile)),
            "the server's client: {reply:?}"
        );
        assert_eq!(
            lock(&server.daemon.state).registry.all().count(),
            2,
            "no record added"
        );

        // A client whose record knows its thread already but not the
        // profile learns the profile, and the server may then act as it.
        let thread = "019a0000-0000-7000-8000-000000000004";
        let bound = server.connected_terminal(thread, true);
        let half = server
            .record(
                bound,
                [
                    ("via".to_owned(), "hook".to_owned()),
                    ("session_id".to_owned(), thread.to_owned()),
                ]
                .into(),
            )
            .await;
        let reply = server.send(true, server.register(bound, thread)).await;
        assert!(
            matches!(&reply, Response::Agent { agent }
                if agent.id == half.id
                    && agent.spec.labels.get(agentdocker_core::agent::PROFILE_LABEL)
                        == Some(&profile)),
            "a client bound to its thread: {reply:?}"
        );
        let claim = serde_json::json!({
            "op": "claim",
            "agent": half.id,
            "resource": format!("path:{}/c", server.workdir.display()),
            "ttl_secs": 60,
        })
        .to_string();
        let reply = server.send(true, claim).await;
        assert!(matches!(reply, Response::Lease { .. }), "{reply:?}");
    }

    /// Over the real socket, between the check and the commit: the shared
    /// server's registration of a terminal nobody holds is admitted, then
    /// paused before the lock that commits it, and meanwhile the terminal
    /// gets a record of its own that names no profile (an MCP-first one)
    /// with a lease of its own. Resumed, the registration is refused
    /// `identity_mismatch` against the agent that holds the process now:
    /// the record learns neither the session nor the profile, keeps its
    /// lease, and the server still cannot act as it.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_record_that_appears_after_the_check_is_not_taken_over() {
        let mut server = SharedServer::start().await;
        let b = server.terminal(THREAD);
        let hold = Arc::new(tokio::sync::Barrier::new(2));
        *lock(&server.daemon.register_hold) = Some(hold.clone());
        let request = server.register(b, THREAD);
        let own = format!("path:{}/own", server.workdir.display());
        let (reply, record) = tokio::join!(server.send(true, request), async {
            // The shared server's registration has been let through.
            hold.wait().await;
            let record = server
                .record(b, [("via".to_owned(), "mcp".to_owned())].into())
                .await;
            let lease = server
                .daemon
                .handle(Request::Claim {
                    agent: record.id.to_string(),
                    resource: own.clone(),
                    mode: Default::default(),
                    amount: None,
                    ttl_secs: 60,
                    note: None,
                    wait_secs: 0,
                    priority: 0,
                    automatic: false,
                })
                .await;
            assert!(matches!(lease, Response::Lease { .. }), "{lease:?}");
            hold.wait().await;
            record
        });
        assert!(identity_mismatch(&reply), "{reply:?}");
        assert!(
            matches!(&reply, Response::Error { details: Some(details), .. }
                if details["agent"] == record.id.as_str() && details["pid"] == b),
            "{reply:?}"
        );
        {
            let state = lock(&server.daemon.state);
            assert_eq!(state.registry.all().count(), 1, "only B's own record");
            let current = state.registry.get(&record.id).unwrap();
            assert_eq!(current.spec.labels, record.spec.labels, "{current:?}");
            assert_eq!(state.leases.len(), 1, "B keeps its lease");
        }
        let claim = serde_json::json!({
            "op": "claim",
            "agent": record.id,
            "resource": format!("path:{}/taken", server.workdir.display()),
            "ttl_secs": 60,
        })
        .to_string();
        let reply = server.send(true, claim).await;
        assert!(identity_mismatch(&reply), "{reply:?}");
        assert_eq!(lock(&server.daemon.state).leases.len(), 1);
    }

    /// Over the real socket: a Codex conversation known by its key alone
    /// (B: profile P, thread T, its checkout), holding a lease and with a
    /// message queued, is not taken by a process that registers itself under
    /// B's labels. C, a child of another agent, may not act as B; its
    /// registration of itself names its own process, so the pid check lets
    /// it through, but it would join B, and is refused `identity_mismatch`
    /// with nothing done: B keeps no process, its lease and its message, no
    /// record is made for C, and C still may not act as B. C may still
    /// register itself for a conversation nobody holds, the person may join
    /// a keyed conversation from outside every agent's session, and from
    /// under P's proven shared server the terminal resuming T joins B under
    /// B's own id.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_keyed_conversation_is_joined_only_from_its_profile_s_server_or_by_the_person() {
        use std::io::{BufRead as _, Write as _};
        let mut server = SharedServer::start().await;
        let daemon = server.daemon.clone();
        let profile = server.profile.display().to_string();
        let attach = |thread: &'static str| {
            let daemon = daemon.clone();
            let profile = profile.clone();
            let workdir = server.workdir.clone();
            async move {
                match daemon
                    .handle(Request::SessionAttach {
                        runtime: "codex".into(),
                        profile,
                        session: thread.into(),
                        workdir,
                        adapter: agentdocker_core::AdapterKind::Mcp,
                    })
                    .await
                {
                    Response::Agent { agent } => agent,
                    other => panic!("{other:?}"),
                }
            }
        };
        let b = attach(THREAD).await;
        assert!(b.pid.is_none() && b.keyed_only(), "{b:?}");
        let lease = daemon
            .handle(Request::Claim {
                agent: b.id.to_string(),
                resource: format!("path:{}/b", server.workdir.display()),
                mode: Default::default(),
                amount: None,
                ttl_secs: 600,
                note: None,
                wait_secs: 0,
                priority: 0,
                automatic: false,
            })
            .await;
        assert!(matches!(lease, Response::Lease { .. }), "{lease:?}");
        let sent = daemon
            .handle(Request::Send {
                from: HUMAN.into(),
                to: b.id.to_string(),
                kind: "chat".into(),
                payload: json!("for B alone"),
                reply_to: None,
                links: Vec::new(),
            })
            .await;
        assert!(!matches!(sent, Response::Error { .. }), "{sent:?}");
        let queued = |id: AgentId| {
            let daemon = daemon.clone();
            async move {
                match daemon
                    .handle(Request::PeekInput {
                        agent: id.to_string(),
                    })
                    .await
                {
                    Response::Messages { messages } => messages.len(),
                    other => panic!("{other:?}"),
                }
            }
        };
        assert_eq!(queued(b.id.clone()).await, 1);

        // C, under agent A's shell: a claim as B, a registration of itself
        // under B's labels, the claim again, and a registration of itself for
        // a conversation nobody holds.
        let impostor = "import json,os,socket,sys\ndef ask(request):\n    s=socket.socket(socket.AF_UNIX)\n    s.connect(sys.argv[1])\n    s.sendall(json.dumps(request).encode()+b'\\n')\n    return s.makefile().readline()\nclaim={'op':'claim','agent':sys.argv[2],'resource':sys.argv[3],'ttl_secs':60}\nfor request in [claim,{'op':'register','spec':json.loads(sys.argv[4]),'pid':os.getpid()},claim,{'op':'register','spec':json.loads(sys.argv[5]),'pid':os.getpid()}]:\n    print('reply:'+ask(request),end='',flush=True)\n";
        let spec = |name: &str, thread: &str| {
            serde_json::json!({
                "name": name,
                "runtime": "codex",
                "workdir": server.workdir,
                "labels": {"session_id": thread, (agentdocker_core::agent::PROFILE_LABEL): profile},
            })
            .to_string()
        };
        let mut a_shell = std::process::Command::new("sh")
            .arg("-c")
            .arg("read go; python3 -c \"$0\" \"$1\" \"$2\" \"$3\" \"$4\" \"$5\"; exit $?")
            .arg(impostor)
            .arg(&daemon.socket)
            .arg(b.id.as_str())
            .arg(format!("path:{}/taken", server.workdir.display()))
            .arg(spec("impostor", THREAD))
            .arg(spec("fresh", "019a0000-0000-7000-8000-000000000009"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let a = server
            .record(a_shell.id(), [("via".to_owned(), "mcp".to_owned())].into())
            .await;
        let agents = lock(&daemon.state).registry.all().count();
        writeln!(a_shell.stdin.take().unwrap(), "go").unwrap();
        let output = tokio::task::spawn_blocking(move || a_shell.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        let replies: Vec<Response> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix("reply:"))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(replies.len(), 4, "{replies:?}");
        assert!(
            identity_mismatch(&replies[0]),
            "C acts as B: {:?}",
            replies[0]
        );
        assert!(
            matches!(&replies[1], Response::Error { code: ErrorCode::IdentityMismatch, details: Some(details), .. }
                if details["agent"] == b.id.as_str()),
            "C joins B: {:?}",
            replies[1]
        );
        assert!(
            identity_mismatch(&replies[2]),
            "C acts as B after: {:?}",
            replies[2]
        );
        assert!(
            matches!(&replies[3], Response::Agent { agent }
                if agent.id != b.id && agent.id != a.id && agent.pid.is_some()),
            "C's own conversation: {:?}",
            replies[3]
        );
        {
            let state = lock(&daemon.state);
            assert_eq!(state.registry.all().count(), agents + 1, "only C's own");
            let current = state.registry.get(&b.id).unwrap();
            assert!(current.pid.is_none(), "{current:?}");
            assert_eq!(current.spec.labels, b.spec.labels);
            assert_eq!(state.leases.by_holder(&b.id).len(), 1, "B keeps its lease");
        }
        assert_eq!(queued(b.id.clone()).await, 1, "B keeps its message");

        // The person, outside every agent's session, joins another keyed
        // conversation with a process of their own.
        let thread = "019a0000-0000-7000-8000-000000000004";
        let persons = attach(thread).await;
        let register_self = "import json,os,socket,sys,time\nparent=os.getppid()\nprint('ready',flush=True)\nwhile os.getppid()==parent:\n    time.sleep(0.01)\ns=socket.socket(socket.AF_UNIX)\ns.connect(sys.argv[1])\ns.sendall(json.dumps({'op':'register','spec':json.loads(sys.argv[2]),'pid':os.getpid()}).encode()+b'\\n')\nprint(s.makefile().readline(),end='',flush=True)\n";
        let mut orphan = std::process::Command::new("sh")
            .arg("-c")
            .arg("python3 -c \"$0\" \"$1\" \"$2\" & read line; exit 0")
            .arg(register_self)
            .arg(&daemon.socket)
            .arg(spec("persons", thread))
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
        assert!(
            matches!(&reply, Response::Agent { agent } if agent.id == persons.id && agent.pid.is_some()),
            "the person joins: {reply:?}"
        );

        // From under P's proven shared server, the terminal resuming T
        // joins B under B's id.
        let terminal = server.terminal(THREAD);
        let reply = server.send(true, server.register(terminal, THREAD)).await;
        assert!(
            matches!(&reply, Response::Agent { agent }
                if agent.id == b.id && agent.pid == Some(terminal)),
            "the profile's server: {reply:?}"
        );
    }

    /// A process outside every agent's session (its parent exits before it
    /// speaks, as a terminal the person opened is): runs `script` with
    /// `args` and answers its last printed line.
    #[cfg(unix)]
    async fn from_outside(script: &str, args: Vec<String>) -> Response {
        use std::io::{BufRead as _, Write as _};
        let mut parent = std::process::Command::new("sh")
            .arg("-c")
            .arg("python3 -c \"$0\" \"$@\" & read line; exit 0")
            .arg(format!(
                "import os,time\nparent=os.getppid()\nprint('ready',flush=True)\nwhile os.getppid()==parent:\n    time.sleep(0.01)\n{script}"
            ))
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        tokio::task::spawn_blocking(move || {
            let mut out = std::io::BufReader::new(parent.stdout.take().unwrap());
            let mut line = String::new();
            out.read_line(&mut line).unwrap();
            assert_eq!(line.trim(), "ready");
            writeln!(parent.stdin.take().unwrap(), "exit").unwrap();
            parent.wait().unwrap();
            line.clear();
            out.read_line(&mut line).unwrap();
            serde_json::from_str::<Response>(&line).unwrap()
        })
        .await
        .unwrap()
    }

    /// Over the real socket: an ended session (B, a Claude Code session with
    /// a role and a message queued) resumes only from outside every other
    /// agent's session, or for the person. C, the child of live agent A,
    /// may not act as B; its registration of itself under B's session would
    /// resume B, as would its registration of A's process, which joins A's
    /// record, and each is refused `identity_mismatch` with nothing done: B
    /// stays ended with its process, role and message, A names no session,
    /// no record is made under B's session, and C still may not act as B. A
    /// process outside every agent's session, as the person's own `claude
    /// --resume` is, resumes B under B's id; and the person, registering a
    /// process that is not their own, resumes another ended session. A
    /// record that already names its session is answered as it is, folding
    /// nothing; and a hook joining the record its session's MCP server made
    /// first is refused, whatever its process's command line says.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_ended_session_resumes_only_from_outside_every_other_agent_s_session() {
        use std::io::Write as _;
        let tmp = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::open(tmp.path().into(), tmp.path().join("host.sock")).unwrap();
        daemon.person_check = false;
        daemon.agent_check = true;
        let daemon = Arc::new(daemon);
        let listener = crate::server::bind(&daemon).await.unwrap();
        let serving = tokio::spawn(crate::server::serve(daemon.clone(), listener));
        let workdir = std::fs::canonicalize(tmp.path()).unwrap();
        let register = |name: String, pid: u32, labels: BTreeMap<String, String>| {
            let daemon = daemon.clone();
            let workdir = workdir.clone();
            async move {
                match daemon
                    .handle(Request::Register {
                        spec: AgentSpec {
                            name,
                            runtime: "claude-code".into(),
                            workdir: Some(workdir),
                            labels,
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
        // A Claude Code session that ended: its record holds a role and a
        // message, its process has exited.
        let ended = |session: &'static str| {
            let daemon = daemon.clone();
            async move {
                let mut process = std::process::Command::new("sleep")
                    .arg("30")
                    .spawn()
                    .unwrap();
                let record = register(
                    format!("claude-{session}"),
                    process.id(),
                    [("session_id".to_owned(), session.to_owned())].into(),
                )
                .await;
                let roles = daemon
                    .handle(Request::RoleAssign {
                        from: HUMAN.into(),
                        agent: record.id.to_string(),
                        add: vec!["reviewer".into()],
                        remove: Vec::new(),
                    })
                    .await;
                assert!(!matches!(roles, Response::Error { .. }), "{roles:?}");
                let sent = daemon
                    .handle(Request::Send {
                        from: HUMAN.into(),
                        to: record.id.to_string(),
                        kind: "chat".into(),
                        payload: json!("for B alone"),
                        reply_to: None,
                        links: Vec::new(),
                    })
                    .await;
                assert!(!matches!(sent, Response::Error { .. }), "{sent:?}");
                let _ = process.kill();
                let _ = process.wait();
                let gone = daemon
                    .handle(Request::Deregister {
                        agent: record.id.to_string(),
                    })
                    .await;
                assert!(matches!(gone, Response::Agent { .. }), "{gone:?}");
                lock(&daemon.state)
                    .registry
                    .get(&record.id)
                    .unwrap()
                    .clone()
            }
        };
        let queued = |id: AgentId| {
            let daemon = daemon.clone();
            async move {
                match daemon
                    .handle(Request::PeekInput {
                        agent: id.to_string(),
                    })
                    .await
                {
                    Response::Messages { messages } => messages.len(),
                    other => panic!("{other:?}"),
                }
            }
        };
        let spec = |name: &str, session: &str| {
            serde_json::json!({
                "name": name,
                "runtime": "claude-code",
                "workdir": workdir,
                "labels": {"session_id": session},
            })
            .to_string()
        };
        let b = ended("session-b").await;
        assert!(!b.status.is_live());
        // The message, and the notice of the role.
        let messages = queued(b.id.clone()).await;
        assert!(messages >= 1);

        // C, under live agent A's shell: a claim as B, a registration of
        // itself under B's session, and the claim again.
        let impostor = "import json,os,socket,sys\ndef ask(request):\n    s=socket.socket(socket.AF_UNIX)\n    s.connect(sys.argv[1])\n    s.sendall(json.dumps(request).encode()+b'\\n')\n    return s.makefile().readline()\nclaim={'op':'claim','agent':sys.argv[2],'resource':sys.argv[3],'ttl_secs':60}\nfor request in [claim,{'op':'register','spec':json.loads(sys.argv[4]),'pid':os.getpid()},{'op':'register','spec':json.loads(sys.argv[4]),'pid':os.getppid()},claim]:\n    print('reply:'+ask(request),end='',flush=True)\n";
        let mut a_shell = std::process::Command::new("sh")
            .arg("-c")
            .arg("read go; python3 -c \"$0\" \"$1\" \"$2\" \"$3\" \"$4\"; exit $?")
            .arg(impostor)
            .arg(&daemon.socket)
            .arg(b.id.as_str())
            .arg(format!("path:{}/taken", workdir.display()))
            .arg(spec("impostor", "session-b"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let a = register("agent-a".into(), a_shell.id(), BTreeMap::new()).await;
        let agents = lock(&daemon.state).registry.all().count();
        writeln!(a_shell.stdin.take().unwrap(), "go").unwrap();
        let output = tokio::task::spawn_blocking(move || a_shell.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        let replies: Vec<Response> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix("reply:"))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(replies.len(), 4, "{replies:?}");
        assert!(
            identity_mismatch(&replies[0]),
            "C acts as B: {:?}",
            replies[0]
        );
        for (reply, what) in [(&replies[1], "itself"), (&replies[2], "A's process")] {
            assert!(
                matches!(reply, Response::Error { code: ErrorCode::IdentityMismatch, details: Some(details), .. }
                    if details["agent"] == b.id.as_str() && details["inside"] == a.id.as_str()),
                "C resumes B, registering {what}: {reply:?}"
            );
        }
        assert!(
            identity_mismatch(&replies[3]),
            "C acts as B after: {:?}",
            replies[3]
        );
        {
            let state = lock(&daemon.state);
            assert_eq!(state.registry.all().count(), agents, "no record for C");
            assert_eq!(
                state
                    .registry
                    .all()
                    .filter(|r| r.spec.labels.get("session_id").map(String::as_str)
                        == Some("session-b"))
                    .count(),
                1,
                "only B names B's session"
            );
            let current = state.registry.get(&b.id).unwrap();
            assert_eq!(current, &b, "B is as it was");
            let a_now = state.registry.get(&a.id).unwrap();
            assert!(
                !a_now.spec.labels.contains_key("session_id"),
                "A learns no session"
            );
            assert_eq!(a_now.status, a.status);
        }
        assert_eq!(queued(b.id.clone()).await, messages, "B keeps its messages");

        // As the person's resumed session, from outside every agent's
        // session, a process registering itself resumes B under B's id.
        let register_self = "import json,os,socket,sys\ns=socket.socket(socket.AF_UNIX)\ns.connect(sys.argv[1])\ns.sendall(json.dumps({'op':'register','spec':json.loads(sys.argv[2]),'pid':int(sys.argv[3]) if len(sys.argv)>3 else os.getpid()}).encode()+b'\\n')\nprint(s.makefile().readline(),end='',flush=True)\n";
        let reply = from_outside(
            register_self,
            vec![
                daemon.socket.display().to_string(),
                spec("resumed", "session-b"),
            ],
        )
        .await;
        assert!(
            matches!(&reply, Response::Agent { agent }
                if agent.id == b.id && agent.status.is_live() && agent.pid != b.pid),
            "the person's resumed session: {reply:?}"
        );
        assert_eq!(queued(b.id.clone()).await, messages, "with its messages");
        assert_eq!(
            lock(&daemon.state)
                .registry
                .get(&b.id)
                .unwrap()
                .spec
                .labels
                .get(agentdocker_core::agent::ROLE_LABEL),
            b.spec.labels.get(agentdocker_core::agent::ROLE_LABEL),
            "and its role"
        );

        // The person registers a process not their own, resuming another.
        let b2 = ended("session-b2").await;
        let mut stranger = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let reply = from_outside(
            register_self,
            vec![
                daemon.socket.display().to_string(),
                spec("persons", "session-b2"),
                stranger.id().to_string(),
            ],
        )
        .await;
        assert!(
            matches!(&reply, Response::Agent { agent }
                if agent.id == b2.id && agent.pid == Some(stranger.id())),
            "the person's registration: {reply:?}"
        );
        let _ = stranger.kill();
        let _ = stranger.wait();

        // A live record that already names its session while an earlier
        // life of that session has ended: a registration from inside it is
        // answered with the record as it is, and folds nothing.
        let mut e_shell = std::process::Command::new("sh")
            .arg("-c")
            .arg("read go; python3 -c \"$0\" \"$1\" \"$2\" $$; exit $?")
            .arg(register_self)
            .arg(&daemon.socket)
            .arg(spec("hook", "session-e"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let bound = register(
            "agent-e".into(),
            e_shell.id(),
            [("session_id".to_owned(), "session-e".to_owned())].into(),
        )
        .await;
        let e = ended("session-e").await;
        writeln!(e_shell.stdin.take().unwrap(), "go").unwrap();
        let output = tokio::task::spawn_blocking(move || e_shell.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        let reply: Response = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            matches!(&reply, Response::Agent { agent } if agent.id == bound.id),
            "the record it is inside: {reply:?}"
        );
        assert_eq!(lock(&daemon.state).registry.get(&e.id).unwrap(), &e);

        // A command line is whatever its process was started with, so it is
        // no evidence either: a process staged as `claude --resume session-c`
        // (any script under a path naming Claude Code) has a record its MCP
        // server made first, naming no session, and its hook joining that
        // record under session C is refused like any other from inside it.
        if !std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            eprintln!("node is not installed here; skipping the staged resumption");
            serving.abort();
            return;
        }
        let c = ended("session-c").await;
        let cli = workdir.join("node_modules/@anthropic-ai/claude-code/cli.js");
        std::fs::create_dir_all(cli.parent().unwrap()).unwrap();
        std::fs::write(
            &cli,
            "const {execFileSync}=require('child_process');\nrequire('fs').readSync(0,Buffer.alloc(1));\nconst [socket,script,spec]=process.argv.slice(4);\nprocess.stdout.write(execFileSync('python3',['-c',script,socket,spec,String(process.pid)]));\n",
        )
        .unwrap();
        let mut staged = std::process::Command::new("node")
            .arg(&cli)
            .args(["--resume", "session-c"])
            .arg(&daemon.socket)
            .arg(register_self)
            .arg(spec("hook", "session-c"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let first = register("mcp-first".into(), staged.id(), BTreeMap::new()).await;
        writeln!(staged.stdin.take().unwrap(), "go").unwrap();
        let output = tokio::task::spawn_blocking(move || staged.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        let reply: Response = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            matches!(&reply, Response::Error { code: ErrorCode::IdentityMismatch, details: Some(details), .. }
                if details["agent"] == c.id.as_str() && details["inside"] == first.id.as_str()),
            "a staged command line resumes C: {reply:?}"
        );
        {
            let state = lock(&daemon.state);
            assert_eq!(state.registry.get(&c.id).unwrap(), &c, "C is as it was");
            assert!(
                !state
                    .registry
                    .get(&first.id)
                    .unwrap()
                    .spec
                    .labels
                    .contains_key("session_id"),
                "and the record learns no session"
            );
        }
        serving.abort();
    }

    /// Over the real socket: an adoption's pid is checked as a
    /// registration's. From inside agent A's session a process may adopt one
    /// it started, and not one it may not name, which is refused
    /// `identity_mismatch` with nothing made; the person, outside every
    /// agent's session as the app is, adopts it.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_adoption_names_only_a_process_its_sender_may_speak_for() {
        use std::io::Write as _;
        let tmp = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::open(tmp.path().into(), tmp.path().join("host.sock")).unwrap();
        daemon.person_check = false;
        daemon.agent_check = true;
        let daemon = Arc::new(daemon);
        let listener = crate::server::bind(&daemon).await.unwrap();
        let serving = tokio::spawn(crate::server::serve(daemon.clone(), listener));
        let mut stranger = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let adopter = "import json,socket,subprocess,sys\ndef ask(request):\n    s=socket.socket(socket.AF_UNIX)\n    s.connect(sys.argv[1])\n    s.sendall(json.dumps(request).encode()+b'\\n')\n    return s.makefile().readline()\nprint('reply:'+ask({'op':'adopt','pid':int(sys.argv[2])}),end='',flush=True)\nchild=subprocess.Popen(['sleep','30'],stdout=subprocess.DEVNULL)\nprint('reply:'+ask({'op':'adopt','pid':child.pid}),end='',flush=True)\nchild.kill()\nchild.wait()\n";
        let mut a_shell = std::process::Command::new("sh")
            .arg("-c")
            .arg("read go; python3 -c \"$0\" \"$1\" \"$2\"; exit $?")
            .arg(adopter)
            .arg(&daemon.socket)
            .arg(stranger.id().to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let Response::Agent { .. } = daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "agent-a".into(),
                    workdir: Some(tmp.path().to_owned()),
                    ..AgentSpec::default()
                },
                pid: Some(a_shell.id()),
                session: None,
            })
            .await
        else {
            panic!("registers")
        };
        writeln!(a_shell.stdin.take().unwrap(), "go").unwrap();
        let output = tokio::task::spawn_blocking(move || a_shell.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        let replies: Vec<Response> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix("reply:"))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(replies.len(), 2, "{replies:?}");
        assert!(
            matches!(&replies[0], Response::Error { code: ErrorCode::IdentityMismatch, details: Some(details), .. }
                if details["pid"] == stranger.id()),
            "a stranger's process: {:?}",
            replies[0]
        );
        assert!(
            matches!(&replies[1], Response::Agent { .. }),
            "its own child: {:?}",
            replies[1]
        );
        assert!(
            !lock(&daemon.state)
                .registry
                .all()
                .any(|record| record.pid == Some(stranger.id())),
            "nothing made for the stranger"
        );
        let adopt = "import json,socket,sys\ns=socket.socket(socket.AF_UNIX)\ns.connect(sys.argv[1])\ns.sendall(json.dumps({'op':'adopt','pid':int(sys.argv[2])}).encode()+b'\\n')\nprint(s.makefile().readline(),end='',flush=True)\n";
        let reply = from_outside(
            adopt,
            vec![
                daemon.socket.display().to_string(),
                stranger.id().to_string(),
            ],
        )
        .await;
        assert!(
            matches!(&reply, Response::Agent { agent } if agent.pid == Some(stranger.id())),
            "the person adopts it: {reply:?}"
        );
        let _ = stranger.kill();
        let _ = stranger.wait();
        serving.abort();
    }
}
