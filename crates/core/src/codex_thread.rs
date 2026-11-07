//! What a Codex rollout says of its own thread, read from the records that
//! carry it and nothing else: no prompt, response or tool text leaves here.
//!
//! A rollout is the JSONL file Codex writes per thread under its profile's
//! `sessions` directory. Its first record is `session_meta`; every turn
//! then writes a `token_usage_record` with the provider's count for that
//! response, and a cumulative `token_count` event after it. The shapes
//! below are tolerant: a field that is missing or of another type reads as
//! unknown, and fields this build does not know are ignored. Each field
//! says whether it was observed in rollouts or only in the binary's
//! strings.

use serde::Deserialize;
use serde_json::Value;

/// A thread id, role, nickname or path as a label: short, printable, one
/// line.
fn label(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
}

/// `session_meta.payload`, the fields read here. Observed.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SessionMeta {
    /// The thread id. Observed.
    id: Option<String>,
    /// Where the thread came from: a word (`cli`, `vscode`, `exec`, `mcp`)
    /// for a thread a person or a client started, an object for one
    /// another thread spawned. Both observed.
    source: Option<Source>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Source {
    Structured(StructuredSource),
    /// A word naming where a root thread came from, or a shape this build
    /// does not know: either way not a spawn.
    Other(serde::de::IgnoredAny),
}

/// The object form of `source`. Observed with `subagent`; the binary's
/// strings also name `memory_consolidation` and `guardian` sources, which
/// this build does not read.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct StructuredSource {
    subagent: Option<SubAgent>,
}

/// `source.subagent`. Observed with `thread_spawn`; the strings name other
/// sub-agent sources this build does not read.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SubAgent {
    thread_spawn: Option<ThreadSpawn>,
}

/// `source.subagent.thread_spawn`: how a thread another spawned describes
/// it. Every field observed in a rollout; `agent_role` observed null in
/// one and named in the binary's strings as the role column.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ThreadSpawn {
    parent_thread_id: Option<String>,
    depth: Option<u64>,
    agent_nickname: Option<String>,
    agent_role: Option<String>,
}

/// What a rollout's first record says of its thread.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadMeta {
    /// The thread's own id.
    pub thread: String,
    /// The role the spawning agent gave it, when one spawned it and said.
    pub role: Option<String>,
    /// Its nickname among the spawning agent's threads.
    pub nickname: Option<String>,
    /// The thread that spawned it; `None` for a root thread.
    pub parent_thread: Option<String>,
    /// How many spawns deep it is; `None` for a root thread.
    pub depth: Option<u64>,
}

impl ThreadMeta {
    /// Whether another thread spawned this one.
    pub fn is_child(&self) -> bool {
        self.parent_thread.is_some()
    }
}

/// What a `session_meta` record says of its thread; `None` for any other
/// record, or one without a thread id.
pub fn session_meta(record: &Value) -> Option<ThreadMeta> {
    if record.get("type")?.as_str()? != "session_meta" {
        return None;
    }
    let payload: SessionMeta = serde_json::from_value(record.get("payload")?.clone()).ok()?;
    let thread = label(payload.id)?;
    let spawn = match payload.source {
        Some(Source::Structured(source)) => source.subagent.and_then(|s| s.thread_spawn),
        _ => None,
    };
    let spawn = spawn.unwrap_or_default();
    Some(ThreadMeta {
        thread,
        role: label(spawn.agent_role),
        nickname: label(spawn.agent_nickname),
        parent_thread: label(spawn.parent_thread_id),
        depth: spawn.depth,
    })
}

/// `token_usage_record.payload`, the fields read here. All observed.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct UsagePayload {
    thread_id: Option<String>,
    turn_id: Option<String>,
    response_id: Option<String>,
    /// The provider's count for this one response.
    usage: Option<Value>,
}

/// The provider's own count for one response of a thread, as a
/// `token_usage_record` carries it: the ids that place it and the raw
/// counter object, which the usage adapter checks field by field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageRecord {
    pub thread: Option<String>,
    pub turn: Option<String>,
    pub response: Option<String>,
    pub usage: Option<Value>,
}

/// What a `token_usage_record` says; `None` for any other record.
pub fn usage_record(record: &Value) -> Option<UsageRecord> {
    if record.get("type")?.as_str()? != "token_usage_record" {
        return None;
    }
    let payload: UsagePayload = serde_json::from_value(record.get("payload")?.clone()).ok()?;
    Some(UsageRecord {
        thread: label(payload.thread_id),
        turn: label(payload.turn_id),
        response: label(payload.response_id),
        usage: payload.usage.filter(Value::is_object),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_root_threads_meta_has_no_parent_and_a_named_source_is_not_a_spawn() {
        let meta = session_meta(&json!({"type":"session_meta","timestamp":"2026-10-02T04:16:11Z",
            "payload":{"id":"thread-root","cli_version":"0.164.0","source":"cli","cwd":"/work/app"}}))
        .unwrap();
        assert_eq!(meta.thread, "thread-root");
        assert!(!meta.is_child());
        assert_eq!(meta.role, None);
        assert_eq!(meta.depth, None);
    }

    #[test]
    fn a_spawned_threads_meta_carries_its_role_nickname_and_parent() {
        let meta = session_meta(&json!({"type":"session_meta","payload":{"id":"thread-child",
            "cli_version":"0.164.0",
            "source":{"subagent":{"thread_spawn":{"parent_thread_id":"thread-root","depth":1,
                "agent_nickname":"Quill","agent_role":"reviewer","agent_path":"/work/app/.codex/agents/reviewer.toml",
                "a_field_this_build_does_not_know":true}}}}}))
        .unwrap();
        assert_eq!(meta.parent_thread.as_deref(), Some("thread-root"));
        assert_eq!(meta.role.as_deref(), Some("reviewer"));
        assert_eq!(meta.nickname.as_deref(), Some("Quill"));
        assert_eq!(meta.depth, Some(1));
        assert!(meta.is_child());
    }

    #[test]
    fn unknown_shapes_read_as_unknown_never_as_an_error() {
        // A null role is a thread spawned without one.
        let meta = session_meta(&json!({"type":"session_meta","payload":{"id":"t",
            "source":{"subagent":{"thread_spawn":{"parent_thread_id":"p","agent_role":null}}}}}))
        .unwrap();
        assert_eq!(meta.role, None);
        assert_eq!(meta.parent_thread.as_deref(), Some("p"));
        // A source of a shape this build does not know is not a spawn.
        let meta = session_meta(&json!({"type":"session_meta","payload":{"id":"t",
            "source":{"subagent":{"other":{"parent_thread_id":"p"}}}}}))
        .unwrap();
        assert!(!meta.is_child());
        let meta =
            session_meta(&json!({"type":"session_meta","payload":{"id":"t","source":7}})).unwrap();
        assert!(!meta.is_child());
        // No id, another record, or a label that is not one line: nothing.
        assert!(
            session_meta(&json!({"type":"session_meta","payload":{"cli_version":"0.164.0"}}))
                .is_none()
        );
        assert!(session_meta(&json!({"type":"turn_context","payload":{"id":"t"}})).is_none());
        assert!(
            session_meta(&json!({"type":"session_meta","payload":{"id":"line\nbreak"}})).is_none()
        );
        let meta = session_meta(&json!({"type":"session_meta","payload":{"id":"t",
            "source":{"subagent":{"thread_spawn":{"parent_thread_id":"p","agent_role":"x".repeat(300)}}}}}))
        .unwrap();
        assert_eq!(meta.role, None);
    }

    #[test]
    fn a_usage_record_names_its_thread_turn_and_response_with_its_count() {
        let usage = json!({"input_tokens":12,"cached_input_tokens":4,"cache_write_input_tokens":0,
            "output_tokens":3,"reasoning_output_tokens":1,"total_tokens":15});
        let record = usage_record(&json!({"type":"token_usage_record","timestamp":"2026-10-02T04:16:12Z",
            "payload":{"thread_id":"thread-root","turn_id":"turn-1","response_id":"resp-1","root_turn_id":"turn-1",
                "session_id":"thread-root","usage":usage,"turn_token_usage":usage,"thread_token_usage":usage}}))
        .unwrap();
        assert_eq!(record.thread.as_deref(), Some("thread-root"));
        assert_eq!(record.turn.as_deref(), Some("turn-1"));
        assert_eq!(record.response.as_deref(), Some("resp-1"));
        assert_eq!(record.usage, Some(usage));
        let bare =
            usage_record(&json!({"type":"token_usage_record","payload":{"usage":"not an object"}}))
                .unwrap();
        assert_eq!(
            bare,
            UsageRecord {
                thread: None,
                turn: None,
                response: None,
                usage: None
            }
        );
        assert!(
            usage_record(&json!({"type":"event_msg","payload":{"type":"token_count"}})).is_none()
        );
    }
}
