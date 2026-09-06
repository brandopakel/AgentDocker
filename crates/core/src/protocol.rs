//! Wire protocol between clients and `agentd`.
//!
//! Transport is newline-delimited JSON over a Unix socket. A client writes
//! one [`Request`] per line and reads one or more [`Response`] lines. Most
//! requests get exactly one response. Streaming requests (`subscribe`,
//! `events`, `logs`) keep sending responses until the client closes the
//! connection or the daemon sends [`Response::End`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::handoff::HandoffBundle;
use crate::journal::{Digest, SummarySource};
use crate::{
    AgentRecord, Change, DiscoveredProcess, Envelope, Event, JournalEntry, Lease, LeaseId,
    LeaseMode, MessageId, VcsState,
};

pub const DEFAULT_LEASE_TTL_SECS: u64 = 300;

/// The digest form of `journal`; see [`crate::journal::digest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestRequest {
    /// An agent reference, or `user` for the human's own cursor.
    pub reader: String,
    pub max_entries: usize,
    pub max_chars: usize,
    #[serde(default)]
    pub all_branches: bool,
    /// Move the reader's cursor to the digest's head in the same request.
    #[serde(default)]
    pub advance: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Ping,
    BuildImage {
        spec: crate::ImageBuildSpec,
    },
    Images,
    WorktreeCreate {
        agent: String,
        path: String,
        branch: String,
    },
    WorktreeDiff {
        agent: String,
    },
    Integrate {
        agent: String,
        source: String,
        validation: String,
        #[serde(default)]
        apply: bool,
    },
    /// Only accepted as the first frame on the separate restricted endpoint.
    Authenticate {
        token: String,
    },
    GrantAccess {
        agent: String,
        container_root: String,
        #[serde(default = "access_ttl")]
        ttl_secs: u64,
    },
    RevokeAccess {
        grant: String,
    },
    Checkpoint {
        agent: String,
        key: String,
        task: String,
        #[serde(default)]
        assumptions: Vec<String>,
        #[serde(default)]
        next_steps: Vec<String>,
        #[serde(default)]
        release_leases: bool,
    },
    /// Review a handoff; acknowledgement is explicit and refuses changed content.
    Resume {
        agent: String,
        checkpoint: String,
        #[serde(default)]
        acknowledge: bool,
    },
    Checkpoints {
        #[serde(default)]
        agent: Option<String>,
    },
    /// Hand work to another agent: a checkpoint addressed to `to` with the
    /// sender's state bundled around it, announced to `to` as a `handoff`
    /// message. Without `to` the bundle is an export nobody is addressed
    /// to yet. Leases are released unless `transfer_leases`, which moves
    /// them at acceptance instead.
    Handoff {
        agent: String,
        #[serde(default)]
        to: Option<String>,
        #[serde(default)]
        task: Option<String>,
        #[serde(default)]
        note: Option<String>,
        #[serde(default)]
        transfer_leases: bool,
        /// Retries with the same key return the same bundle.
        #[serde(default)]
        key: Option<String>,
    },
    /// Bundles sent by or addressed to an agent; every bundle without one.
    Handoffs {
        #[serde(default)]
        agent: Option<String>,
    },
    /// Bring a bundle exported on another host here, addressed to `agent`
    /// and re-homed to its checkout; `resume` then accepts it as usual.
    Import {
        agent: String,
        bundle: Box<HandoffBundle>,
    },
    Validate {
        agent: String,
        command: Vec<String>,
        #[serde(default = "validation_timeout")]
        timeout_secs: u64,
    },
    Validations {
        agent: String,
    },
    /// Record content immediately before reading these paths (not after a delayed tool result).
    Observe {
        agent: String,
        paths: Vec<String>,
    },
    /// Compare retained reads with current content. Querying never clears staleness.
    Stale {
        agent: String,
        #[serde(default)]
        paths: Vec<String>,
    },
    Reads {
        agent: String,
    },

    /// Spawn `spec.command` and supervise it.
    Run {
        spec: crate::AgentSpec,
    },
    /// Launch a command inside a retained immutable image, with optional scoped mounts.
    RunContainer {
        spec: crate::AgentSpec,
        build: String,
        #[serde(default)]
        options: crate::container::ContainerRunOptions,
    },
    /// Replace a managed container after observing its exit; returns a new agent ID.
    RestartContainer {
        agent: String,
    },
    /// Announce an already-running process (e.g. a Claude Code session).
    Register {
        spec: crate::AgentSpec,
        #[serde(default)]
        pid: Option<u32>,
    },
    /// Mark an externally managed agent as finished.
    Deregister {
        agent: String,
    },
    /// Running agent processes (known runtimes) that nobody registered.
    Discover,
    /// Register a running process found by `discover`, by pid.
    Adopt {
        pid: u32,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        runtime: Option<String>,
    },
    /// Signal a managed agent to stop (SIGTERM, or SIGKILL when `force`).
    Stop {
        agent: String,
        #[serde(default)]
        force: bool,
    },
    /// Forget a finished agent.
    Remove {
        agent: String,
    },
    List {
        #[serde(default)]
        all: bool,
        /// A project id (any unique prefix), or an absolute path inside the
        /// project.
        #[serde(default)]
        project: Option<String>,
        /// Only agents carrying every one of these labels.
        #[serde(default)]
        labels: BTreeMap<String, String>,
    },
    Inspect {
        agent: String,
    },
    Heartbeat {
        agent: String,
    },
    /// What an adapter observed about its agent. Everything is optional;
    /// the daemon keeps what changed and emits events for it.
    Report {
        agent: String,
        #[serde(default)]
        vcs: Option<VcsState>,
    },
    /// Ledger entries for a project: newest `limit`, oldest first.
    Changes {
        /// A project id (any unique prefix), or an absolute path inside it.
        project: String,
        #[serde(default)]
        since_seq: Option<u64>,
        /// A path (absolute, or relative to the checkout); a directory
        /// matches everything beneath it.
        #[serde(default)]
        path: Option<String>,
        /// Only changes attributed to this agent.
        #[serde(default)]
        agent: Option<String>,
        #[serde(default = "default_changes_limit")]
        limit: usize,
    },
    /// Ask the daemon to exit: managed agents get SIGTERM, as on Ctrl-C.
    Shutdown,

    /// Publish a message. `to` uses [`crate::Destination::parse`] shorthand.
    Send {
        from: String,
        to: String,
        kind: String,
        payload: Value,
        #[serde(default)]
        reply_to: Option<MessageId>,
    },
    /// Stream messages for `agent` and/or matching `topics` until the
    /// connection closes. Queued inbox messages are flushed first.
    Subscribe {
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        topics: Vec<String>,
    },
    /// Messages delivered to an agent while it was not subscribed.
    Inbox {
        agent: String,
        #[serde(default)]
        drain: bool,
    },

    /// Idempotently remove only messages that a consumer has delivered.
    AckInbox {
        agent: String,
        messages: Vec<MessageId>,
    },

    Claim {
        agent: String,
        resource: String,
        #[serde(default)]
        mode: LeaseMode,
        #[serde(default = "default_ttl")]
        ttl_secs: u64,
        #[serde(default)]
        note: Option<String>,
        /// Seconds to wait for a conflicting lease to clear before giving
        /// up; 0 reports the conflict immediately.
        #[serde(default)]
        wait_secs: u64,
    },
    Renew {
        agent: String,
        lease: LeaseId,
        #[serde(default = "default_ttl")]
        ttl_secs: u64,
    },
    Release {
        agent: String,
        lease: LeaseId,
        /// What changed and why, for the journal; synthesised from the
        /// ledger when absent.
        #[serde(default)]
        summary: Option<String>,
        /// `explicit` (the default) is journaled even when nothing was
        /// held; `transcript` only describes leases actually released.
        #[serde(default)]
        summary_source: SummarySource,
    },
    /// Release every lease an agent holds; the reply lists them.
    ReleaseAll {
        agent: String,
        #[serde(default)]
        summary: Option<String>,
        #[serde(default)]
        summary_source: SummarySource,
    },
    /// Append a free-text note to the journal of the agent's project.
    JournalAdd {
        agent: String,
        summary: String,
    },
    /// Journal entries for a project: newest `limit` matching, oldest
    /// first. `project` is an id prefix or an absolute path inside it.
    Journal {
        project: String,
        #[serde(default)]
        since_seq: Option<u64>,
        #[serde(default)]
        until_seq: Option<u64>,
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        branch: Option<String>,
        #[serde(default)]
        kind: Option<String>,
        /// A path (absolute, or relative to the checkout); a directory
        /// matches entries touching anything beneath it.
        #[serde(default)]
        path: Option<String>,
        /// Full-text search over summaries.
        #[serde(default)]
        grep: Option<String>,
        #[serde(default = "default_changes_limit")]
        limit: usize,
        /// Render a digest for a reader instead of listing: what is after
        /// the reader's cursor (or `since_seq` when given), within a
        /// budget. Only the project and `since_seq` apply alongside it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        digest: Option<DigestRequest>,
    },
    /// Paths changed in more than one physical checkout of a project:
    /// what will collide when the branches meet. With `agent`, only the
    /// overlaps involving that agent's checkout; an empty `project` then
    /// means the agent's own.
    Overlap {
        project: String,
        #[serde(default)]
        since_seq: Option<u64>,
        #[serde(default)]
        agent: Option<String>,
    },
    /// Drop journal entries of a project below `before_seq`.
    JournalPrune {
        project: String,
        before_seq: u64,
    },
    Leases {
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        resource: Option<String>,
    },

    /// Replay the last `replay` stored events, then stream new ones until
    /// the connection closes.
    Events {
        #[serde(default)]
        replay: usize,
        /// Send EventsReady once subscribed, before replay or live events.
        #[serde(default)]
        ready: bool,
    },
    /// Replay the last `tail` log lines of an agent, then keep streaming
    /// while `follow` and the agent is alive.
    Logs {
        agent: String,
        #[serde(default)]
        follow: bool,
        #[serde(default = "default_tail")]
        tail: usize,
    },
}

fn default_ttl() -> u64 {
    DEFAULT_LEASE_TTL_SECS
}

fn default_tail() -> usize {
    100
}

fn default_changes_limit() -> usize {
    50
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NotFound,
    Ambiguous,
    Conflict,
    NameTaken,
    Forbidden,
    Invalid,
    Internal,
    StorageUnavailable,
    /// A part of the daemon is off — the restricted container endpoint
    /// could not be served — so what needs it is refused, not broken.
    Unavailable,
    EngineUnavailable,
    BuildFailed,
}

// A response is built once and serialised at once, so the size gap between
// `Agent` (a whole record) and `Ok` never matters; boxing would only add
// ceremony at every construction and match site.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    ImageBuild {
        build: crate::ImageBuild,
    },
    ImageBuilds {
        builds: Vec<crate::ImageBuild>,
    },
    Worktree {
        path: std::path::PathBuf,
        branch: String,
    },
    Diff {
        text: String,
    },
    Integration {
        source_head: String,
        applied: bool,
        clean: bool,
        text: String,
    },
    Access {
        grant: String,
        token: String,
        socket: std::path::PathBuf,
        expires_at: chrono::DateTime<chrono::Utc>,
    },

    Checkpoint {
        checkpoint: crate::Checkpoint,
    },
    Checkpoints {
        checkpoints: Vec<crate::Checkpoint>,
    },
    Handoff {
        bundle: HandoffBundle,
    },
    Handoffs {
        bundles: Vec<HandoffBundle>,
    },
    Recovery {
        recovery: crate::Recovery,
    },
    Validation {
        validation: crate::Validation,
        passed: bool,
    },
    Validations {
        validations: Vec<crate::Validation>,
    },

    Reads {
        reads: Vec<crate::ReadMark>,
    },
    Stale {
        stale: Vec<crate::StalePath>,
    },

    Pong {
        version: String,
        uptime_secs: u64,
        /// The restricted container endpoint's socket while it is serving;
        /// absent when it is off or still starting.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        restricted: Option<std::path::PathBuf>,
    },
    Agent {
        agent: AgentRecord,
    },
    Agents {
        agents: Vec<AgentRecord>,
    },
    Processes {
        processes: Vec<DiscoveredProcess>,
    },
    Changes {
        changes: Vec<Change>,
    },
    Overlap {
        overlaps: Vec<crate::Overlap>,
    },
    /// `subscribers` is how many live subscriptions were notified; queued
    /// inbox delivery is not counted.
    Sent {
        message: MessageId,
        subscribers: usize,
    },
    Message {
        message: Envelope,
    },
    Messages {
        messages: Vec<Envelope>,
    },
    Lease {
        lease: Lease,
    },
    Leases {
        leases: Vec<Lease>,
    },
    Journal {
        project: crate::ProjectId,
        entries: Vec<JournalEntry>,
    },
    JournalEntry {
        entry: JournalEntry,
    },
    Digest {
        project: crate::ProjectId,
        digest: Digest,
    },
    Pruned {
        removed: usize,
    },
    /// The requested events subscription is active; a snapshot can now begin.
    EventsReady,
    Event {
        event: Event,
    },
    Log {
        line: String,
    },
    /// A live stream skipped messages; events can be recovered with replay.
    Lagged {
        skipped: u64,
    },
    Ok,
    /// Terminates a stream that finished on the daemon's side.
    End,
    Error {
        code: ErrorCode,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
    },
}

impl Response {
    pub fn error(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Error {
            code,
            message: message.into(),
            details: None,
        }
    }
}

fn validation_timeout() -> u64 {
    300
}

fn access_ttl() -> u64 {
    3600
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip() {
        let json = r#"{"op":"claim","agent":"reviewer","resource":"path:/repo/src"}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        assert_eq!(
            req,
            Request::Claim {
                agent: "reviewer".into(),
                resource: "path:/repo/src".into(),
                mode: LeaseMode::Exclusive,
                ttl_secs: DEFAULT_LEASE_TTL_SECS,
                note: None,
                wait_secs: 0,
            }
        );
        let back = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&back).unwrap(), req);
    }

    #[test]
    fn ping_is_just_an_op() {
        assert_eq!(
            serde_json::to_string(&Request::Ping).unwrap(),
            r#"{"op":"ping"}"#
        );
        assert_eq!(
            serde_json::to_string(&Response::End).unwrap(),
            r#"{"type":"end"}"#
        );
    }

    #[test]
    fn struct_variants_accept_missing_defaults() {
        let req: Request = serde_json::from_str(r#"{"op":"events"}"#).unwrap();
        assert_eq!(
            req,
            Request::Events {
                replay: 0,
                ready: false
            }
        );
    }
    #[test]
    fn lease_acquisition_sequence_is_optional_on_wire_and_round_trips() {
        let value = serde_json::json!({"id":"lease", "resource":"task:test", "holder":"agent", "mode":"exclusive",
            "acquired_at":"2026-09-05T00:00:00Z", "expires_at":"2026-09-05T00:01:00Z"});
        let mut lease: Lease = serde_json::from_value(value).unwrap();
        assert_eq!(lease.change_seq, None);
        assert!(
            serde_json::to_value(&lease)
                .unwrap()
                .get("change_seq")
                .is_none()
        );
        lease.change_seq = Some(42);
        let response = Response::Lease { lease };
        let wire = serde_json::to_value(&response).unwrap();
        assert_eq!(wire["lease"]["change_seq"], 42);
        assert_eq!(serde_json::from_value::<Response>(wire).unwrap(), response);
    }
}
