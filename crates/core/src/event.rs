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
