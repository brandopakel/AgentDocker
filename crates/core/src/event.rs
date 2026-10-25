//! Events emitted by the daemon, consumed by `agentdocker events`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    AgentId, AgentStatus, Change, ChannelId, Destination, JournalEntry, Lease, MessageId,
    ProjectId, ProjectRef, ResourceKey, VcsState,
};

/// An opaque position in one durable event log. Retain the complete cursor:
/// a sequence alone cannot distinguish another database or rewritten history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventCursor {
    pub log: String,
    pub seq: u64,
    pub digest: String,
}

impl EventCursor {
    pub fn is_valid(&self) -> bool {
        let hex = |value: &str, len: usize| {
            value.len() == len
                && value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        hex(&self.log, 32)
            && self.seq <= i64::MAX as u64
            && if self.seq == 0 {
                self.digest.is_empty()
            } else {
                hex(&self.digest, 64)
            }
    }
}

/// Bounds apply to the checked stream, including the response envelope.
pub const EVENT_STREAM_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const EVENT_REPLAY_LIMIT: usize = 4096;

/// How a wait finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitOutcome {
    /// The waiter got the lease.
    Claimed,
    /// It ran out of the time it asked for.
    Timeout,
    /// Its connection went away, so the request no longer exists.
    Cancelled,
    /// It would have closed a cycle.
    Deadlock,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventKind {
    /// A completed local output write was counted separately from provider tokens.
    UsageEmissionRecorded {
        agent: AgentId,
        emission: crate::usage::emission::Emission,
    },
    /// A bounded usage batch, its accounting and scan progress committed.
    UsageRecorded {
        generation: u64,
        samples: u64,
        gaps: u64,
    },
    /// Retained unattributed contributions moved without re-ingestion.
    UsageReconciled {
        agent: AgentId,
        samples: u64,
    },
    /// Volatile review metadata changed. Live-only seq:0, no prompt, answer or capability.
    SecretReviewChanged {
        review: String,
        agent: AgentId,
    },
    /// A question's answer route is durable until answered or expired.
    QuestionOpened {
        question: MessageId,
        expires_at: DateTime<Utc>,
    },
    /// No answer means the question expired. The original inbox message remains.
    QuestionClosed {
        question: MessageId,
        answer: Option<MessageId>,
    },
    /// How an answer reached its asker: handed over as the result of the
    /// synchronous `ask` that was waiting for it (and taken out of the
    /// asker's queue), or left in the queue because no `ask` was waiting
    /// or the one that was ended without it.
    AnswerRouted {
        question: MessageId,
        answer: MessageId,
        route: crate::AnswerRoute,
    },
    /// The asker closed an unanswered question. Inbox messages remain.
    QuestionCancelled {
        question: MessageId,
        agent: AgentId,
    },
    AgentActivityReported {
        agent: AgentId,
        observation: crate::ActivityObservation,
    },
    /// A person gave an agent a name; `name` is the new one.
    AgentRenamed {
        agent: AgentId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<ProjectId>,
        name: String,
    },
    /// Who started a session that registered itself, as its process's
    /// parents say: `by` is `user` (the person), `agent` or `unknown`. Its
    /// record now carries `launched_by` = `by`, and keeps it.
    AgentLaunchedBy {
        agent: AgentId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<ProjectId>,
        by: String,
    },
    /// An agent was given a role, or `None` when it was taken away.
    RoleSet {
        agent: AgentId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<ProjectId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<String>,
    },
    AdapterContactReported {
        agent: AgentId,
        adapter: crate::AdapterKind,
        contact: crate::AdapterContact,
    },
    InputDeliveryReported {
        agent: AgentId,
        delivery: crate::InputDelivery,
    },
    /// The agent's receiver is holding its next input while the person
    /// types in its terminal, and decides again at `until`
    /// ([`crate::typing`]). The hold ends with a receipt, a pause or the
    /// time passing.
    InputHeldForTyping {
        agent: AgentId,
        until: DateTime<Utc>,
    },
    /// An external controller became the sole consumer of the agent's
    /// queued input, or resumed a binding it held before.
    InputBound {
        agent: AgentId,
        controller: crate::ProcessIdentity,
        resumed: bool,
    },
    /// The binding ended; legacy readers may consume the queue again.
    InputUnbound {
        agent: AgentId,
        reason: String,
    },
    /// The bound controller, or a process launched to replace it, is gone.
    InputControllerEnded {
        agent: AgentId,
        controller: crate::ProcessIdentity,
    },
    /// The daemon started the binding's launch descriptor; the process
    /// binds itself with the controller's token.
    InputControllerLaunched {
        agent: AgentId,
        controller: crate::ProcessIdentity,
        attempt: u32,
    },
    /// A launch attempt failed before the process started.
    InputControllerLaunchFailed {
        agent: AgentId,
        attempt: u32,
        error: String,
    },
    /// The episode's launches are used up; the binding stands, dead,
    /// until a controller binds by itself or somebody unbinds it.
    InputRestartsExhausted {
        agent: AgentId,
        attempts: u32,
    },
    /// A person asked for the controller to be started again: the episode
    /// starts over, the next launch is due at once.
    InputRestartsReset {
        agent: AgentId,
    },
    /// A person's explicit receiver upgrade committed before stopping the old
    /// controller. The provider, binding token and queue stay unchanged.
    InputControllerUpgraded {
        agent: AgentId,
        controller: crate::ProcessIdentity,
        previous_executable: std::path::PathBuf,
        executable: std::path::PathBuf,
    },
    /// A provider session that came back as a new process was joined to
    /// the record that holds its thread's queue and binding; the new
    /// record's id is an alias of it from now on.
    InputResumed {
        agent: AgentId,
        retired: AgentId,
        provider: crate::ProviderGeneration,
    },
    /// A provider session that came back as a new process, without an input
    /// binding, was joined to the record that ended with it: the new
    /// process's registration is retired into the old record, which keeps
    /// its id, its conversations and its queue, and takes the new pid.
    SessionResumed {
        agent: AgentId,
        retired: Vec<AgentId>,
        session: String,
        pid: u32,
    },
    /// Several live records of one provider session in one process — the
    /// same process birth, runtime, project and session — were made one:
    /// the others were retired into `agent` exactly as a resumed session's
    /// are (queue, documents, observations, aliases), and their ids are
    /// aliases of it from now on. No longer emitted: a session has one
    /// record from registration, so nothing folds live duplicates. Kept so
    /// stored events and older clients decode.
    SessionDuplicatesFolded {
        agent: AgentId,
        retired: Vec<AgentId>,
        session: String,
        pid: u32,
    },
    /// An ended session is being brought back by this daemon as a process
    /// it supervises, under the same record; `agent_started` follows when
    /// the process is up, `agent_exited`/failed if it never comes.
    SessionRelaunched {
        agent: AgentId,
        session: String,
    },
    ProviderAvailabilityReported {
        agent: AgentId,
        availability: crate::ProviderAvailability,
        user_resumed: bool,
    },
    ContainerUpdated {
        agent: AgentId,
    },
    ImageBuilt {
        build: String,
        engine: crate::ContainerEngine,
        image_id: String,
    },
    WorktreeCreated {
        agent: crate::AgentId,
        path: std::path::PathBuf,
    },
    /// A worktree the daemon created has a post-commit hook that reports
    /// the commits made there, and a witness token only its reports carry;
    /// `agent` is who it was created for.
    WorktreeWitnessed {
        agent: crate::AgentId,
        path: std::path::PathBuf,
    },
    /// The daemon recorded that an agent made a commit on a branch (its
    /// full ref), kept apart from the journal so that the agent's word is
    /// not taken on that branch.
    AuthorshipRecorded {
        project: ProjectId,
        agent: crate::AgentId,
        branch: String,
        commit: String,
        witness: crate::gate::Witness,
    },
    /// An agent committed its checkout through the daemon.
    Committed {
        agent: crate::AgentId,
        head: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        files: usize,
        pushed: bool,
    },
    /// The person merged an agent's branch into the branch in the
    /// project's main checkout.
    BranchMerged {
        agent: crate::AgentId,
        branch: String,
        target: String,
        commit: String,
    },
    WorktreeCleanup {
        agent: crate::AgentId,
        path: std::path::PathBuf,
        worktree_removed: bool,
        branch_removed: bool,
        reason: Option<String>,
    },
    IntegrationPrepared {
        agent: crate::AgentId,
        source_head: String,
        clean: bool,
    },
    AccessGranted {
        agent: crate::AgentId,
        grant: String,
    },
    AccessRevoked {
        grant: String,
    },
    CheckpointSaved {
        agent: crate::AgentId,
        checkpoint: String,
    },
    HandoffAccepted {
        agent: crate::AgentId,
        checkpoint: String,
    },
    /// A handoff bundle was made; `to` is absent for an export.
    HandoffSent {
        from: AgentId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<AgentId>,
        handoff: String,
    },
    /// A bundle from another host was brought here for `agent`.
    HandoffImported {
        agent: AgentId,
        handoff: String,
    },
    /// A lease moved to a handoff's recipient at acceptance.
    LeaseTransferred {
        lease: Lease,
        from: AgentId,
        to: AgentId,
    },
    ValidationStarted {
        agent: AgentId,
        validation: String,
    },
    ValidationFinished {
        agent: crate::AgentId,
        validation: String,
        passed: bool,
    },
    WatcherGap {
        reason: String,
    },
    /// The daemon is spawning its project watcher; registrations wait for
    /// it rather than go unwatched.
    WatcherStarting,
    /// The watcher is up: checkouts are covered from registration on.
    WatcherStarted,
    /// The watcher could not start; the ledger and branch tracking are off
    /// until the daemon restarts.
    WatcherUnavailable {
        reason: String,
    },
    /// The restricted container endpoint is serving on this socket.
    RestrictedEndpointListening {
        socket: std::path::PathBuf,
    },
    /// The restricted container endpoint could not be served; the host
    /// socket keeps working and new grants are refused.
    RestrictedEndpointUnavailable {
        reason: String,
    },
    ReadsObserved {
        agent: crate::AgentId,
        paths: Vec<std::path::PathBuf>,
    },
    AgentStale {
        agent: crate::AgentId,
        paths: Vec<std::path::PathBuf>,
    },
    InboxAcknowledged {
        agent: crate::AgentId,
        messages: Vec<crate::MessageId>,
    },
    /// What the person said to everyone in a project shortly before an
    /// agent opened there, queued for it as its first input.
    MessagesCaughtUp {
        agent: crate::AgentId,
        messages: Vec<crate::MessageId>,
    },
    /// A process of a known agent runtime appeared that no registered
    /// agent claims; `adopt` makes it one.
    AgentDiscovered {
        pid: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_at: Option<DateTime<Utc>>,
        runtime: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<ProjectId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<std::path::PathBuf>,
    },
    /// A discovered process is no longer unregistered: it exited, or it
    /// was adopted.
    AgentVanished {
        pid: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_at: Option<DateTime<Utc>>,
        runtime: String,
        adopted: bool,
    },
    /// A process scan failed; the previous snapshot is retained, not exited.
    DiscoveryUnavailable {
        reason: String,
    },
    /// Scanning recovered and a fresh snapshot is available.
    DiscoveryAvailable,
    AgentCreated {
        agent: AgentId,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<ProjectId>,
    },
    /// The human's working location changed durably.
    HumanLocationChanged {
        agent: AgentId,
        workdir: std::path::PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<ProjectRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vcs: Option<VcsState>,
    },
    AgentStarted {
        agent: AgentId,
        pid: Option<u32>,
    },
    /// A record that named no session has been shown whose it is.
    ///
    /// One process is one agent, and the two halves of a session do not
    /// arrive together: whichever registers first owns the record, and
    /// only the hooks adapter knows the session id. When the other half
    /// arrives the record learns it — which is what stops the *next*
    /// session in the same process from adopting the same identity.
    AgentSessionBound {
        agent: AgentId,
        session: String,
    },
    /// Maintenance committed an exact-ID redirect and all operational moves.
    /// Original history and before-images remain available in the repair archive.
    AgentReconciled {
        canonical: AgentId,
        retired: AgentId,
        plan_sha256: String,
    },
    /// A task several agents will attempt, with the measure that ranks
    /// them fixed before any of them starts.
    ContestOpened {
        contest: crate::ContestId,
        task: String,
        measure: String,
        entrants: Vec<AgentId>,
    },
    ContestEntered {
        contest: crate::ContestId,
        agent: AgentId,
    },
    /// An attempt with passing evidence behind it.
    ContestSubmitted {
        contest: crate::ContestId,
        agent: AgentId,
        validation: String,
        /// Rendered, because `EventKind` is comparable and a float is
        /// not. The number itself lives in the contest record, which is
        /// where anyone ranking things reads it.
        score: String,
    },
    /// The answer, and whether the metric settled it or a person did.
    ContestClosed {
        contest: crate::ContestId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        winner: Option<AgentId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolution: Option<String>,
    },
    /// A claim could not be satisfied and took a place in the queue.
    /// `position` is how many waiters are served before it and in its way
    /// (higher priority first, then arrival); zero means it is next.
    LeaseWaiting {
        resource: crate::ResourceKey,
        requester: AgentId,
        position: usize,
    },
    /// A wait ended: the lease was taken, the caller gave up, its
    /// connection went away, or it was the victim of a cycle.
    LeaseWaitEnded {
        resource: crate::ResourceKey,
        requester: AgentId,
        outcome: WaitOutcome,
    },
    /// A claim's wait would have closed a cycle, and the cycle was broken
    /// at one wait in it (`crate::wait::victim`): the lowest priority, the
    /// youngest among equals, the claim that found it counting as the
    /// youngest of all. That claim is refused, or an older wait is ended
    /// and the claim looks again.
    LeaseDeadlock {
        cycle: Vec<crate::Blocked>,
        /// The step of `cycle` whose wait was ended: who, waiting for
        /// what, held by whom. Absent in events from a daemon predating
        /// it, where the newcomer was always the victim.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        victim: Option<crate::Blocked>,
    },
    /// A card was filed on a project's board.
    TaskCreated {
        task: crate::TaskId,
        project: ProjectId,
        by: String,
        title: String,
    },
    /// An agent took a Ready card: theirs, in progress.
    TaskPulled {
        task: crate::TaskId,
        project: ProjectId,
        agent: AgentId,
        /// Whom the card was taken over from, when the pull was an
        /// explicit recovery of a hold that had lapsed (the lease
        /// expired, was released, or its agent exited); the holder
        /// itself, re-taking its own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<AgentId>,
        /// The `task:<id>` lease the pull took; it is the holding.
        lease: crate::LeaseId,
    },
    /// A card moved lanes, by its assignee or the person.
    TaskMoved {
        task: crate::TaskId,
        project: ProjectId,
        by: String,
        column: crate::Column,
        /// The lane it went to, by slug.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lane: Option<String>,
    },
    /// A card's words, fields or holder changed: which, by name.
    TaskUpdated {
        task: crate::TaskId,
        project: ProjectId,
        by: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        fields: Vec<String>,
    },
    /// A card left the board.
    TaskArchived {
        task: crate::TaskId,
        project: ProjectId,
        by: String,
    },
    /// A card came back on the board.
    TaskUnarchived {
        task: crate::TaskId,
        project: ProjectId,
        by: String,
    },
    /// Somebody said something on a card.
    TaskCommented {
        task: crate::TaskId,
        project: ProjectId,
        by: String,
        comment: crate::CommentId,
    },
    /// A comment was rewritten, or deleted.
    TaskCommentChanged {
        task: crate::TaskId,
        project: ProjectId,
        by: String,
        comment: crate::CommentId,
        deleted: bool,
    },
    /// A board was made, renamed or reshaped.
    BoardChanged {
        project: ProjectId,
        board: String,
        by: String,
    },
    /// A board's rule was made, changed or deleted.
    RuleChanged {
        project: ProjectId,
        board: String,
        rule: String,
        by: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        deleted: bool,
    },
    /// A rule acted on a card (what it did has its own event too).
    RuleFired {
        project: ProjectId,
        rule: String,
        task: crate::TaskId,
    },
    /// A project's tag was made, recoloured, renamed or deleted.
    TagChanged {
        project: ProjectId,
        name: String,
        by: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        renamed_to: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        deleted: bool,
    },
    /// A role in a project's palette was made, changed or taken out.
    RoleChanged {
        project: ProjectId,
        role: String,
        by: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        deleted: bool,
        /// The tools its holders are now offered, when this change set
        /// them; empty when it lifted the limit.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tools: Option<Vec<String>>,
    },
    /// A memory was kept in a project.
    MemoryAdded {
        project: ProjectId,
        memory: String,
        by: String,
    },
    /// A memory was forgotten for good.
    MemoryForgotten {
        project: ProjectId,
        memory: String,
        by: String,
    },
    /// The copy of a project's memory kept elsewhere caught up: how many
    /// memories it was given and how many it no longer holds.
    MemoryMirrored {
        project: ProjectId,
        added: usize,
        removed: usize,
    },
    /// A plan was written or changed.
    PlanChanged {
        project: ProjectId,
        plan: String,
        by: String,
    },
    /// A project file was kept, linked, unlinked or deleted.
    ProjectFileChanged {
        project: ProjectId,
        file: String,
        by: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        deleted: bool,
    },
    /// A webhook sink gave up on deliveries: which sink (by its configured
    /// name, never its address), the last event kind it could not post,
    /// why in a word (`refused`, `unreachable`, `timeout`, `too_large`,
    /// `dropped`), and how many deliveries were lost since the last such
    /// notice. Never posted to any sink itself.
    WebhookFailed {
        name: String,
        kind: String,
        reason: String,
        dropped: u64,
    },
    /// The opt-in judge read an agent's quiet screen as waiting on the
    /// person: what for, how sure, and which screen of which process (by
    /// digest, never its text). Its record carries the judgment and its
    /// activity reads as waiting, marked judged, until the screen changes
    /// or can no longer be read, its own adapter reports, or another
    /// process is behind it.
    AgentJudgedNeedsYou {
        agent: AgentId,
        judgment: crate::judgment::ScreenJudgment,
    },
    /// A judgment was taken off an agent's record: `screen_changed` (its
    /// screen moved on), `process_changed` (another process is behind
    /// it), `screen_unreadable` (its screen can no longer be read: lost
    /// for a reason of the agent's own, or its source silent for
    /// [`crate::judgment::OUTAGE_PASSES`] passes in a row) or `judge_off`
    /// (the judge was turned off or cannot run), as
    /// [`crate::judgment::ClearReason`] words them.
    AgentJudgmentCleared {
        agent: AgentId,
        reason: String,
    },
    /// The judge was turned on or off, or cannot run: `status` is `on`,
    /// `off` or `unusable`, and `reason` says why it cannot, in words that
    /// name neither the key nor the address. Said only when that changes,
    /// never for a read that leaves it as it was.
    JudgeStatusChanged {
        status: crate::judgment::JudgeState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The judge's calls to its model failed: why in a word
    /// (`unauthorized`, `refused`, `rate_limited`, `unavailable`,
    /// `unreachable`, `timeout`, `malformed`) and how many failed since the
    /// last such notice. At most once a minute; the address and the key
    /// never appear.
    JudgeFailed {
        reason: String,
        failures: u64,
    },
    /// The person told a project's agents to hold, and why.
    ProjectPaused {
        project: ProjectId,
        by: String,
        reason: String,
    },
    /// The person lifted a project's pause.
    ProjectResumed {
        project: ProjectId,
        by: String,
    },
    /// The policy refused something. Carries what was asked and which
    /// rule said no, so a refusal is explainable from the event stream
    /// alone.
    PolicyDenied {
        agent: AgentId,
        action: String,
        rule: String,
    },
    /// A policy's effective rules or load diagnostic changed. Invalid initial
    /// policy denies admission; a later error retains the last good rules.
    PolicyUpdated {
        project: Option<std::path::PathBuf>,
        rules: u64,
        quotas: u64,
        error: Option<String>,
        using_last_good: bool,
    },
    /// A managed agent that exited was started again by its restart
    /// policy, under its own identity. `attempt` counts from one.
    AgentRestarted {
        agent: AgentId,
        pid: Option<u32>,
        attempt: u32,
    },
    /// An explicit stop cleared the agent's restart policy: it will not
    /// come back on its own.
    AgentRestartCleared {
        agent: AgentId,
    },
    /// An explicit stop cleared the agent's restore intent: a daemon
    /// restart will not bring it back.
    AgentRestoreCleared {
        agent: AgentId,
    },
    /// A restarted daemon brought a managed agent back under its own
    /// identity, so everything already recorded about it still applies.
    /// Durable restore intent and lease protection precede process launch.
    AgentRestoring {
        agent: AgentId,
    },
    AgentRestored {
        agent: AgentId,
        pid: Option<u32>,
        /// Paths it had read that have changed since, so the reason to
        /// look at the record is visible in the feed.
        stale: usize,
    },
    /// Stop requested; the process still owns its leases until observed exit.
    AgentStopping {
        agent: AgentId,
        force: bool,
    },
    AgentExited {
        agent: AgentId,
        status: AgentStatus,
    },
    /// Output capture failed; the separate exit event retains the actual
    /// process status. A successful exit does not imply the log is complete.
    AgentOutputFailed {
        agent: AgentId,
        reason: String,
    },
    AgentRemoved {
        agent: AgentId,
    },
    MessageSent {
        message: MessageId,
        from: String,
        to: Destination,
        kind: String,
    },
    LeaseClaimed {
        lease: Lease,
    },
    LeaseRenewed {
        lease: Lease,
    },
    LeaseReleased {
        lease: Lease,
    },
    LeaseExpired {
        lease: Lease,
    },
    LeaseConflict {
        resource: ResourceKey,
        requester: AgentId,
        held_by: Vec<AgentId>,
    },
    /// A repository was seen for the first time on this host.
    ProjectDiscovered {
        project: ProjectRef,
    },
    /// The project watcher saw a file change; the ledger keeps it. Emitted
    /// to the live stream only — persisted in the `changes` table, not the
    /// event history, which change volume would otherwise crowd out.
    FileChanged {
        change: Change,
    },
    /// Agents turned out to be working on the same thing, or somebody
    /// opened a room for a task: they can talk and review there now.
    ChannelOpened {
        channel: crate::ChannelId,
        project: ProjectId,
        title: String,
        members: Vec<AgentId>,
    },
    /// A reader read a conversation through an archived seq.
    ConversationRead {
        reader: AgentId,
        conversation: crate::ConversationId,
        through: u64,
    },
    /// Archived messages were dropped by retention or the cap.
    MessagesPruned {
        removed: usize,
    },
    /// Topic retention dropped kept topic messages past a topic's age or
    /// count limit.
    TopicMessagesPruned {
        removed: usize,
    },
    /// An agent acknowledged kept topic messages: each topic's cursor now
    /// stands at the sequence named, and a later subscribe replays only
    /// what came after it.
    TopicCursorsAdvanced {
        agent: crate::AgentId,
        cursors: Vec<crate::TopicCursor>,
    },
    /// Retention forgot the records of agents that ended before its
    /// window and held nothing; each was said as `agent_removed` first.
    AgentsPruned {
        removed: usize,
    },
    /// Somebody was added to an open channel.
    ChannelInvited {
        channel: ChannelId,
        by: AgentId,
        agent: AgentId,
    },
    ChannelJoined {
        channel: crate::ChannelId,
        agent: AgentId,
    },
    /// The work is final, or everybody left: the channel is done and can
    /// be pruned.
    ChannelClosed {
        channel: crate::ChannelId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolution: Option<String>,
    },
    /// Closed channels were durably removed by housekeeping.
    ChannelsPruned {
        channels: Vec<crate::ChannelId>,
    },
    /// A reviewer gave a verdict on another agent's work in a channel.
    ReviewSubmitted {
        channel: crate::ChannelId,
        by: AgentId,
        of: AgentId,
        verdict: crate::Verdict,
    },
    /// A reviewer gave its word on a branch at the commit it read.
    CodeVerdictGiven {
        project: ProjectId,
        branch: String,
        head: String,
        by: AgentId,
        verdict: crate::Verdict,
    },
    /// A journal entry was appended to a project.
    JournalAppended {
        entry: JournalEntry,
    },
    /// A reader's journal cursor moved: everything up to `seq` has been
    /// shown to it. `reader` is an agent id, or `user` for the human.
    JournalRead {
        reader: String,
        project: ProjectId,
        seq: u64,
    },
    /// A project's journal entries below `before_seq` were deleted, on
    /// request or by the configured retention window (`reason`).
    JournalPruned {
        project: ProjectId,
        before_seq: u64,
        removed: usize,
        reason: String,
    },
    /// A session owner that outlived the previous daemon is supervised
    /// again by this one, with its child untouched.
    AgentOwnerReattached {
        agent: AgentId,
        owner_pid: u32,
    },
    /// Keystrokes an attached client sent were refused by the agent's
    /// session owner because its terminal queue was full; nothing was
    /// typed and nothing is replayed.
    AgentInputDropped {
        agent: AgentId,
        reason: String,
    },
    /// A managed agent's session owner could not be found after a daemon
    /// restart and left no exit report; the agent is recorded as failed.
    AgentOwnerLost {
        agent: AgentId,
        reason: String,
    },
    /// Checkpoints (and the handoff bundles carrying them) older than a
    /// requested age were deleted; only finished sessions' are eligible.
    CheckpointsPruned {
        checkpoints: Vec<String>,
    },
    /// An agent's checkout moved to another branch or commit.
    AgentVcsChanged {
        agent: AgentId,
        vcs: VcsState,
    },
    /// The daemon is about to exit; `reason` is `signal`, `request` or
    /// `transferred`.
    DaemonStopping {
        reason: String,
    },
    /// This daemon stopped writing and offered coordination to a successor
    /// process. Mutating requests answer `transferring` until the transfer
    /// settles.
    DaemonTransferOffered {
        transfer: String,
        successor_pid: u32,
    },
    /// The offer now names the successor process that was actually
    /// started; the offer itself named a placeholder because a pid is
    /// only known once the process exists.
    DaemonTransferReaddressed {
        transfer: String,
        successor_pid: u32,
    },
    /// The successor wrote once and owns the database; this daemon will
    /// exit without touching agents.
    DaemonTransferAccepted {
        transfer: String,
    },
    /// The transfer did not complete; this daemon resumed writing.
    DaemonTransferAborted {
        transfer: String,
        reason: String,
    },
    /// An event this build has never heard of.
    ///
    /// The daemon and its clients are separate binaries and are
    /// routinely at different versions: an upgrade replaces one before
    /// the other, and `daemon reload` swaps the daemon underneath a
    /// desktop window that is already open. Without this, the first new
    /// event kind a newer daemon emits fails to deserialise in an older
    /// client, the error takes the whole stream down, and the app shows
    /// itself as disconnected from a daemon that is working perfectly.
    ///
    /// An old client should ignore what it does not understand, not
    /// fall over. Nothing constructs this; serde produces it for a tag
    /// that matches nothing else.
    #[serde(other)]
    Unknown,
}

impl EventKind {
    /// Every kind's wire name — the `event` tag serde writes, the
    /// variant in snake_case — in the enum's order, once, so a place
    /// that takes kinds by name (`[[webhooks]] events`) can refuse a
    /// name that is not one instead of waiting for an event that never
    /// comes. `unknown` is left out: it is the reader's catch-all for a
    /// newer daemon's kinds and is never emitted. A test holds this list
    /// to the enum in both directions.
    pub const WIRE_NAMES: &'static [&'static str] = &[
        "usage_emission_recorded",
        "usage_recorded",
        "usage_reconciled",
        "secret_review_changed",
        "question_opened",
        "question_closed",
        "answer_routed",
        "question_cancelled",
        "agent_activity_reported",
        "agent_renamed",
        "agent_launched_by",
        "role_set",
        "adapter_contact_reported",
        "input_delivery_reported",
        "input_held_for_typing",
        "input_bound",
        "input_unbound",
        "input_controller_ended",
        "input_controller_launched",
        "input_controller_launch_failed",
        "input_restarts_exhausted",
        "input_restarts_reset",
        "input_controller_upgraded",
        "input_resumed",
        "session_resumed",
        "session_duplicates_folded",
        "session_relaunched",
        "provider_availability_reported",
        "container_updated",
        "image_built",
        "worktree_created",
        "worktree_witnessed",
        "authorship_recorded",
        "committed",
        "branch_merged",
        "worktree_cleanup",
        "integration_prepared",
        "access_granted",
        "access_revoked",
        "checkpoint_saved",
        "handoff_accepted",
        "handoff_sent",
        "handoff_imported",
        "lease_transferred",
        "validation_started",
        "validation_finished",
        "watcher_gap",
        "watcher_starting",
        "watcher_started",
        "watcher_unavailable",
        "restricted_endpoint_listening",
        "restricted_endpoint_unavailable",
        "reads_observed",
        "agent_stale",
        "inbox_acknowledged",
        "messages_caught_up",
        "agent_discovered",
        "agent_vanished",
        "discovery_unavailable",
        "discovery_available",
        "agent_created",
        "human_location_changed",
        "agent_started",
        "agent_session_bound",
        "agent_reconciled",
        "contest_opened",
        "contest_entered",
        "contest_submitted",
        "contest_closed",
        "lease_waiting",
        "lease_wait_ended",
        "lease_deadlock",
        "task_created",
        "task_pulled",
        "task_moved",
        "task_updated",
        "task_archived",
        "task_unarchived",
        "task_commented",
        "task_comment_changed",
        "board_changed",
        "rule_changed",
        "rule_fired",
        "tag_changed",
        "role_changed",
        "memory_added",
        "memory_forgotten",
        "memory_mirrored",
        "plan_changed",
        "project_file_changed",
        "webhook_failed",
        "agent_judged_needs_you",
        "agent_judgment_cleared",
        "judge_status_changed",
        "judge_failed",
        "project_paused",
        "project_resumed",
        "policy_denied",
        "policy_updated",
        "agent_restarted",
        "agent_restart_cleared",
        "agent_restore_cleared",
        "agent_restoring",
        "agent_restored",
        "agent_stopping",
        "agent_exited",
        "agent_output_failed",
        "agent_removed",
        "message_sent",
        "lease_claimed",
        "lease_renewed",
        "lease_released",
        "lease_expired",
        "lease_conflict",
        "project_discovered",
        "file_changed",
        "channel_opened",
        "conversation_read",
        "messages_pruned",
        "topic_messages_pruned",
        "topic_cursors_advanced",
        "agents_pruned",
        "channel_invited",
        "channel_joined",
        "channel_closed",
        "channels_pruned",
        "review_submitted",
        "code_verdict_given",
        "journal_appended",
        "journal_read",
        "journal_pruned",
        "agent_owner_reattached",
        "agent_input_dropped",
        "agent_owner_lost",
        "checkpoints_pruned",
        "agent_vcs_changed",
        "daemon_stopping",
        "daemon_transfer_offered",
        "daemon_transfer_readdressed",
        "daemon_transfer_accepted",
        "daemon_transfer_aborted",
    ];

    /// Whether `name` is the wire name of a kind the daemon emits.
    pub fn is_wire_name(name: &str) -> bool {
        Self::WIRE_NAMES.contains(&name)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Position in the daemon's event log: strictly increasing, assigned by
    /// the daemon when the event is emitted. `0` means not yet assigned.
    #[serde(default)]
    pub seq: u64,
    pub at: DateTime<Utc>,
    pub kind: EventKind,
}

impl Event {
    pub fn new(kind: EventKind, now: DateTime<Utc>) -> Self {
        Self {
            seq: 0,
            at: now,
            kind,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The failure this exists for, seen for real: a daemon was upgraded
    /// under a desktop window that was already open, the window read an
    /// event kind it had never heard of, and the parse error took the
    /// whole stream down — so a working daemon showed as "disconnected
    /// unknown variant `committed`".
    #[test]
    fn an_event_kind_from_a_newer_daemon_is_read_as_unknown_not_an_error() {
        let from_the_future = r#"{"event":"something_invented_later","weight":3}"#;
        let parsed: EventKind = serde_json::from_str(from_the_future)
            .expect("an unfamiliar kind is a kind, not a broken frame");
        assert_eq!(parsed, EventKind::Unknown);

        // And in the envelope the stream actually carries.
        let framed = r#"{"seq":7,"at":"2026-09-07T00:00:00Z","kind":{"event":"not_yet_designed"}}"#;
        let event: Event = serde_json::from_str(framed).expect("the frame still reads");
        assert_eq!(event.seq, 7);
        assert_eq!(event.kind, EventKind::Unknown);
    }

    /// The catch-all must not swallow kinds this build does know: a
    /// variant that quietly stopped matching would be worse than the
    /// disconnect, because nothing would report it.
    #[test]
    fn known_kinds_still_round_trip() {
        for kind in [
            EventKind::DaemonStopping {
                reason: "signal".to_owned(),
            },
            EventKind::WorktreeCreated {
                agent: AgentId::from("a1b2c3"),
                path: std::path::PathBuf::from("/tmp/w"),
            },
            EventKind::InputHeldForTyping {
                agent: AgentId::from("a1b2c3"),
                until: DateTime::from_timestamp(1_700_000_003, 0).unwrap(),
            },
        ] {
            let text = serde_json::to_string(&kind).unwrap();
            let back: EventKind = serde_json::from_str(&text).unwrap();
            assert_eq!(back, kind, "{text}");
            assert_ne!(back, EventKind::Unknown, "{text}");
        }
    }

    #[test]
    fn human_location_optional_fields_accept_legacy_null_and_omission() {
        let omitted = serde_json::json!({
            "event": "human_location_changed", "agent": "human", "workdir": "/project"
        });
        let mut legacy = omitted.clone();
        legacy["project"] = serde_json::Value::Null;
        legacy["vcs"] = serde_json::Value::Null;
        let kind: EventKind = serde_json::from_value(omitted.clone()).unwrap();
        assert_eq!(serde_json::from_value::<EventKind>(legacy).unwrap(), kind);
        assert_eq!(serde_json::to_value(kind).unwrap(), omitted);
    }

    /// The wire names the enum's source declares: each variant of
    /// `EventKind`, in order, in serde's snake_case, without the
    /// catch-all. Line endings are normalised first, since a checkout on
    /// Windows may carry `\r\n` and the markers are matched by line.
    fn wire_names_in(source: &str) -> Vec<String> {
        let source = source.replace("\r\n", "\n");
        let body = source
            .split_once("pub enum EventKind {\n")
            .expect("the enum")
            .1
            .split_once("\n}\n")
            .expect("its end")
            .0;
        body.lines()
            .filter_map(|line| {
                let rest = line.strip_prefix("    ")?;
                if rest.starts_with(' ') || !rest.starts_with(|c: char| c.is_ascii_uppercase()) {
                    return None;
                }
                let variant: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .collect();
                let mut snake = String::new();
                for (i, c) in variant.chars().enumerate() {
                    if c.is_ascii_uppercase() && i > 0 {
                        snake.push('_');
                    }
                    snake.push(c.to_ascii_lowercase());
                }
                Some(snake)
            })
            .filter(|name| name != "unknown")
            .collect()
    }

    /// The failure seen on a Windows runner: Git checked the source out
    /// with `\r\n`, the enum's markers were not found, and the test that
    /// guards the list could not run. The scan reads either ending alike.
    #[test]
    fn the_enum_scan_reads_a_crlf_checkout_the_same() {
        let source = include_str!("event.rs");
        let unix = wire_names_in(source);
        let crlf = source.replace("\r\n", "\n").replace('\n', "\r\n");
        assert!(crlf.contains("\r\n"));
        assert_eq!(wire_names_in(&crlf), unix);
        assert_eq!(unix, EventKind::WIRE_NAMES);
    }

    /// `WIRE_NAMES` is the enum, by name: every variant's snake_case tag
    /// is listed, nothing else is, and nothing twice. The enum's source
    /// is the authority, so the test reads it rather than keeping a
    /// second list.
    #[test]
    fn wire_names_are_the_enum_in_snake_case() {
        let from_source = wire_names_in(include_str!("event.rs"));
        assert!(
            from_source.len() > 100,
            "the enum was found: {}",
            from_source.len()
        );
        assert_eq!(
            EventKind::WIRE_NAMES,
            from_source,
            "WIRE_NAMES follows the enum, variant for variant"
        );
        let mut seen = std::collections::BTreeSet::new();
        for name in EventKind::WIRE_NAMES {
            assert!(seen.insert(*name), "{name} is listed twice");
            // Serde knows each name as a kind: a tag it recognises is read
            // as that variant (or fails on its missing fields), never as
            // the catch-all.
            let probe = format!(r#"{{"event":"{name}"}}"#);
            let parsed = serde_json::from_str::<EventKind>(&probe);
            assert!(
                !matches!(parsed, Ok(EventKind::Unknown)),
                "serde does not know `{name}`"
            );
        }
        assert!(EventKind::is_wire_name("question_opened"));
        assert!(!EventKind::is_wire_name("question_asked"));
        assert!(!EventKind::is_wire_name("unknown"));
        for kind in [
            EventKind::DaemonStopping {
                reason: "signal".to_owned(),
            },
            EventKind::WebhookFailed {
                name: "team".to_owned(),
                kind: "question_opened".to_owned(),
                reason: String::new(),
                dropped: 0,
            },
        ] {
            let tag = serde_json::to_value(&kind).unwrap()["event"]
                .as_str()
                .unwrap()
                .to_owned();
            assert!(EventKind::is_wire_name(&tag), "{tag}");
        }
    }
}
