//! Events emitted by the daemon, consumed by `agentdocker events`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    AgentId, AgentStatus, Change, Destination, JournalEntry, Lease, MessageId, ProjectId,
    ProjectRef, ResourceKey, VcsState,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventKind {
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
    AgentStarted {
        agent: AgentId,
        pid: Option<u32>,
    },
    /// A restarted daemon brought a managed agent back under its own
    /// identity, so everything already recorded about it still applies.
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
    /// Somebody was added to an open channel.
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
    /// A reviewer gave a verdict on another agent's work in a channel.
    ReviewSubmitted {
        channel: crate::ChannelId,
        by: AgentId,
        of: AgentId,
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
    /// An agent's checkout moved to another branch or commit.
    AgentVcsChanged {
        agent: AgentId,
        vcs: VcsState,
    },
    /// The daemon is about to exit; `reason` is `signal` or `request`.
    DaemonStopping {
        reason: String,
    },
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
