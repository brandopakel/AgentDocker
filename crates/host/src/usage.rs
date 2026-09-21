//! Versioned local usage formats. Only accounting metadata leaves the parser;
//! prompt, response and tool-result text is never part of a sample or cursor.

use agentdocker_core::usage::Counters;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub mod discovery;
pub mod reader;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Semantics {
    Cumulative,
    Response,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sample {
    pub source_id: String,
    pub runtime: String,
    pub format: String,
    pub session_id: String,
    pub at: DateTime<Utc>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub counters: Counters,
    pub semantics: Semantics,
    pub proves_zero_baseline: bool,
}

/// A parser cursor is persisted only together with accepted samples. It keeps
/// model context for subsequent cumulative snapshots, never transcript text.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Codex {
    session: Option<String>,
    version: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    from_start: bool,
    saw_usage: bool,
}

fn label(value: &Value) -> Option<String> {
    let value = value.as_str()?;
    (!value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control))
        .then(|| value.to_owned())
}

fn timestamp(value: &Value) -> Result<DateTime<Utc>, String> {
    value
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|at| at.with_timezone(&Utc))
        .ok_or_else(|| "usage timestamp is missing or invalid".into())
}

fn counter(value: &Value, key: &str) -> Result<Option<u64>, String> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("usage counter {key} is not a nonnegative integer")),
    }
}

fn consistent(counters: Counters) -> Result<Counters, String> {
    if counters.values().iter().all(Option::is_none) {
        return Err("usage record contains no supported token counters".into());
    }
    if let (Some(input), Some(read)) = (counters.input_tokens, counters.cache_read_input_tokens) {
        if read > input {
            return Err("cache read tokens exceed total input".into());
        }
    }
    if let (Some(input), Some(write)) = (counters.input_tokens, counters.cache_write_input_tokens) {
        if write > input {
            return Err("cache write tokens exceed total input".into());
        }
    }
    if let (Some(input), Some(read), Some(write)) = (
        counters.input_tokens,
        counters.cache_read_input_tokens,
        counters.cache_write_input_tokens,
    ) {
        if read.checked_add(write).is_none_or(|cached| cached > input) {
            return Err("combined cache tokens exceed total input".into());
        }
    }
    if let (Some(output), Some(reasoning)) =
        (counters.output_tokens, counters.reasoning_output_tokens)
    {
        if reasoning > output {
            return Err("reasoning tokens exceed total output".into());
        }
    }
    Ok(counters)
}

fn codex_counters(value: &Value) -> Result<Counters, String> {
    if !value.is_object() {
        return Err("unsupported Codex token counter object".into());
    }
    consistent(Counters {
        input_tokens: counter(value, "input_tokens")?,
        cache_read_input_tokens: counter(value, "cached_input_tokens")?,
        cache_write_input_tokens: counter(value, "cache_write_input_tokens")?,
        output_tokens: counter(value, "output_tokens")?,
        reasoning_output_tokens: counter(value, "reasoning_output_tokens")?,
    })
}

fn identity(runtime: &str, session: &str, key: &str) -> String {
    let mut hash = Sha256::new();
    for part in [runtime, session, key] {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    format!("{:x}", hash.finalize())
}

impl Codex {
    /// Parse one complete JSONL object. `at_file_start` is true only for the
    /// original first record, not the first record of a resumed scan chunk.
    pub fn feed(&mut self, record: &Value, at_file_start: bool) -> Result<Option<Sample>, String> {
        let payload = &record["payload"];
        match record["type"].as_str() {
            Some("session_meta") => {
                self.version = None;
                let session = label(&payload["id"]).ok_or("Codex session id is missing")?;
                let version =
                    label(&payload["cli_version"]).ok_or("Codex log version is missing")?;
                // Fixtures establish these local formats. Future formats must
                // earn support rather than silently borrowing old semantics.
                if !matches!(version.as_str(), "0.153.4" | "0.154.0") {
                    return Err("unsupported Codex rollout version".into());
                }
                if self.session.as_ref().is_some_and(|old| old != &session) {
                    return Err("Codex rollout changed session identity".into());
                }
                self.session = Some(session);
                self.version = Some(version);
                self.provider = label(&payload["model_provider"]);
                self.from_start = at_file_start && payload.get("forked_from_id").is_none();
                Ok(None)
            }
            Some("turn_context") => {
                self.model = label(&payload["model"]);
                Ok(None)
            }
            Some("event_msg") if payload["type"] == "token_count" => {
                let info = &payload["info"];
                if info.is_null() {
                    return Ok(None);
                }
                if self.version.is_none() {
                    return Err("Codex usage has no supported format context".into());
                }
                let counters = codex_counters(&info["total_token_usage"])?;
                let session = self
                    .session
                    .as_ref()
                    .ok_or("Codex usage precedes session metadata")?;
                let at = timestamp(&record["timestamp"])?;
                let first = self.from_start
                    && !self.saw_usage
                    && codex_counters(&info["last_token_usage"]).ok().as_ref() == Some(&counters);
                self.saw_usage = true;
                // Cumulative records have no provider event ID. Session,
                // timestamp and reported counters identify the observed snapshot
                // independently of its file path, including copied rollouts.
                let key = serde_json::to_string(&(at, &counters)).map_err(|e| e.to_string())?;
                Ok(Some(Sample {
                    source_id: identity("codex", session, &key),
                    runtime: "codex".into(),
                    format: "codex-rollout-0.153.4-0.154.0-v1".into(),
                    session_id: session.clone(),
                    at,
                    provider: self.provider.clone(),
                    model: self.model.clone(),
                    counters,
                    semantics: Semantics::Cumulative,
                    proves_zero_baseline: first,
                }))
            }
            _ => Ok(None),
        }
    }
}

/// Claude repeats one response over multiple content records. Its message ID,
/// not a content-record UUID, is the dedupe key. A conflicting observation of
/// the same response must be reconciled, never added as another response.
pub fn claude(record: &Value) -> Result<Option<Sample>, String> {
    if record["type"] != "assistant" {
        return Ok(None);
    }
    let message = &record["message"];
    let usage = &message["usage"];
    if usage.is_null() {
        return Ok(None);
    }
    if !matches!(
        record["version"].as_str(),
        Some(
            "2.1.268"
                | "2.1.270"
                | "2.1.271"
                | "2.1.272"
                | "2.1.273"
                | "2.1.274"
                | "2.1.275"
                | "2.1.276"
        )
    ) {
        return Err("unsupported Claude transcript version".into());
    }
    if !usage.is_object() {
        return Err("unsupported Claude usage object".into());
    }
    let session = label(&record["sessionId"]).ok_or("Claude usage has no session id")?;
    let id = label(&message["id"]).ok_or("Claude usage has no response id")?;
    let uncached = counter(usage, "input_tokens")?;
    let read = counter(usage, "cache_read_input_tokens")?;
    let write = counter(usage, "cache_creation_input_tokens")?;
    // Claude's input_tokens excludes cached reads and writes. Do not treat
    // omitted cache counters as zero or add subfields of cache_creation again.
    let input = match (uncached, read, write) {
        (Some(a), Some(b), Some(c)) => Some(
            a.checked_add(b)
                .and_then(|n| n.checked_add(c))
                .ok_or("Claude total input overflows")?,
        ),
        _ => None,
    };
    let counters = consistent(Counters {
        input_tokens: input,
        cache_read_input_tokens: read,
        cache_write_input_tokens: write,
        output_tokens: counter(usage, "output_tokens")?,
        reasoning_output_tokens: counter(&usage["output_tokens_details"], "thinking_tokens")?,
    })?;
    Ok(Some(Sample {
        source_id: identity("claude-code", &session, &id),
        runtime: "claude-code".into(),
        // Keep the original accounting-family identity as compatible patch
        // versions are verified; changing it would conflict with saved samples.
        format: "claude-transcript-2.1.268-270-v1".into(),
        session_id: session,
        at: timestamp(&record["timestamp"])?,
        // A model/runtime name does not identify an API gateway or biller.
        provider: label(&record["provider"]),
        model: label(&message["model"]),
        counters,
        semantics: Semantics::Response,
        proves_zero_baseline: false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn codex_uses_cumulative_totals_once_and_retains_only_usage_context() {
        let mut parser = Codex::default();
        parser.feed(&json!({"type":"session_meta","payload":{"id":"thread-a","cli_version":"0.154.0","model_provider":"local-fixture"}}), true).unwrap();
        parser
            .feed(
                &json!({"type":"turn_context","payload":{"model":"model-a","prompt":"PRIVATE"}}),
                false,
            )
            .unwrap();
        let totals = json!({"input_tokens":100,"cached_input_tokens":80,"output_tokens":20,"reasoning_output_tokens":5});
        let record = json!({"type":"event_msg","timestamp":"2026-09-16T12:00:00Z","payload":{"type":"token_count","info":{"total_token_usage":totals,"last_token_usage":totals}}});
        let first = parser.feed(&record, false).unwrap().unwrap();
        assert!(first.proves_zero_baseline);
        assert_eq!(first.counters.input_tokens, Some(100));
        assert_eq!(first.counters.output_tokens, Some(20));
        assert_eq!(first.counters.cache_write_input_tokens, None);
        let saved = serde_json::to_string(&parser).unwrap();
        assert!(!saved.contains("PRIVATE"));
        let mut resumed: Codex = serde_json::from_str(&saved).unwrap();
        let replay = resumed.feed(&record, false).unwrap().unwrap();
        assert_eq!(first.source_id, replay.source_id);
        assert!(!replay.proves_zero_baseline);
        assert_eq!(replay.provider.as_deref(), Some("local-fixture"));
    }

    #[test]
    fn codex_requires_supported_context_and_does_not_invent_initial_coverage() {
        let totals = json!({"input_tokens":100,"cached_input_tokens":80,"output_tokens":20});
        let mut record = json!({"type":"event_msg","timestamp":"2026-09-16T12:00:00Z","payload":{"type":"token_count","info":{"total_token_usage":totals,"last_token_usage":{"input_tokens":1,"output_tokens":1}}}});
        let mut parser = Codex::default();
        assert!(parser.feed(&record, false).is_err());
        for version in ["0.153.4", "0.154.0"] {
            let mut supported = Codex::default();
            supported.feed(&json!({"type":"session_meta","payload":{"id":"thread-a","cli_version":version}}), true).unwrap();
            assert!(
                !supported
                    .feed(&record, false)
                    .unwrap()
                    .unwrap()
                    .proves_zero_baseline
            );
            let unsupported =
                json!({"type":"session_meta","payload":{"id":"thread-a","cli_version":"0.155.0"}});
            assert!(supported.feed(&unsupported, false).is_err());
            assert!(supported.feed(&record, false).is_err());
        }
        parser.feed(&json!({"type":"session_meta","payload":{"id":"thread-a","cli_version":"0.154.0","forked_from_id":"prior"}}), true).unwrap();
        record["payload"]["info"]["last_token_usage"] = totals;
        assert!(
            !parser
                .feed(&record, false)
                .unwrap()
                .unwrap()
                .proves_zero_baseline
        );
        record["payload"]["info"]["total_token_usage"]["cache_write_input_tokens"] = json!(30);
        assert!(parser.feed(&record, false).is_err());
        record["payload"]["info"]["total_token_usage"]["cache_write_input_tokens"] = json!(0);
        record["payload"]["info"]["total_token_usage"]["output_tokens"] = json!(1.5);
        assert!(parser.feed(&record, false).is_err());
    }

    #[test]
    fn claude_caches_are_disjoint_and_content_records_share_one_response_id() {
        let mut record = json!({"type":"assistant","version":"2.1.270","sessionId":"session-a","uuid":"part-one","timestamp":"2026-09-16T12:00:00Z","message":{"id":"message-a","model":"model-a","content":[{"text":"PRIVATE"}],"usage":{"input_tokens":2,"cache_read_input_tokens":30,"cache_creation_input_tokens":15,"output_tokens":9,"output_tokens_details":{"thinking_tokens":3},"cache_creation":{"ephemeral_1h_input_tokens":15}}}});
        for version in [
            "2.1.268", "2.1.270", "2.1.271", "2.1.272", "2.1.273", "2.1.274", "2.1.275", "2.1.276",
        ] {
            record["version"] = json!(version);
            assert!(claude(&record).unwrap().is_some());
        }
        let first = claude(&record).unwrap().unwrap();
        assert_eq!(
            first.counters.values(),
            [Some(47), Some(30), Some(15), Some(9), Some(3)]
        );
        assert_eq!(first.provider, None);
        assert!(!serde_json::to_string(&first).unwrap().contains("PRIVATE"));
        record["uuid"] = json!("part-two");
        record["timestamp"] = json!("2026-09-16T12:00:01Z");
        assert_eq!(first.source_id, claude(&record).unwrap().unwrap().source_id);
        record["message"]["usage"]
            .as_object_mut()
            .unwrap()
            .remove("cache_creation_input_tokens");
        assert_eq!(
            claude(&record).unwrap().unwrap().counters.input_tokens,
            None
        );
        record["message"]["usage"]["output_tokens"] = json!(-1);
        assert!(claude(&record).is_err());
        record["version"] = json!("future");
        assert!(claude(&record).is_err());
    }
}
