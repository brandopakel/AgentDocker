//! Events emitted by the daemon, consumed by `agentdocker events`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    AgentId, AgentStatus, Change, Destination, JournalEntry, Lease, MessageId, ProjectId,
    ProjectRef, ResourceKey, VcsState,
};

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
    AgentActivityReported {
        agent: AgentId,
        observation: crate::ActivityObservation,
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
    /// An agent committed its checkout through the daemon.
    Committed {
        agent: crate::AgentId,
        head: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        files: usize,
        pushed: bool,
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
    /// `position` is how many waiters are ahead of it on an overlapping
    /// resource; zero means it is next.
    LeaseWaiting {
        resource: crate::ResourceKey,
        requester: AgentId,
        position: usize,
    },
    /// A wait ended: the lease was taken, the caller gave up, its
    /// connection went away, or a cycle was found.
    LeaseWaitEnded {
        resource: crate::ResourceKey,
        requester: AgentId,
        outcome: WaitOutcome,
    },
    /// A claim was refused because granting the wait would have closed a
    /// cycle. The newcomer is always the victim: deterministic, and it
    /// needs no priorities.
    LeaseDeadlock {
        cycle: Vec<crate::Blocked>,
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
        ] {
            let text = serde_json::to_string(&kind).unwrap();
            let back: EventKind = serde_json::from_str(&text).unwrap();
            assert_eq!(back, kind, "{text}");
            assert_ne!(back, EventKind::Unknown, "{text}");
        }
    }
}
