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

use crate::{
    AgentRecord, DiscoveredProcess, Envelope, Event, Lease, LeaseId, LeaseMode, MessageId, VcsState,
};

pub const DEFAULT_LEASE_TTL_SECS: u64 = 300;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Ping,

    /// Spawn `spec.command` and supervise it.
    Run {
        spec: crate::AgentSpec,
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
    },
    /// Release every lease an agent holds; the reply lists them.
    ReleaseAll {
        agent: String,
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
}

// A response is built once and serialised at once, so the size gap between
// `Agent` (a whole record) and `Ok` never matters; boxing would only add
// ceremony at every construction and match site.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Pong {
        version: String,
        uptime_secs: u64,
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
    Event {
        event: Event,
    },
    Log {
        line: String,
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
        assert_eq!(req, Request::Events { replay: 0 });
    }
}
