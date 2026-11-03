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
    /// The provider's thread that spawned this sample's session, when the
    /// session's log says one did (a Codex thread another thread spawned):
    /// whose the usage is when the session itself has no agent. Left out of
    /// the serialized sample, and so of its identity, when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
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
    /// The thread that spawned this one, from `session_meta`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
}

/// Codex releases whose rollouts were checked against a fixture.
const CODEX_CHECKED: [&str; 5] = ["0.153.4", "0.154.0", "0.155.1", "0.160.0", "0.164.0"];

/// The format of a Codex sample taken from the provider's own count for one
/// response (`token_usage_record`), whatever the release: the record has
/// read the same since the oldest checked one. A rollout keeps two accounts
/// of a thread's tokens, once per response and once as running totals
/// (`token_count`, the cumulative formats). They agree except where the
/// totals fall short: they do not move for the call that compacts a long
/// conversation, and in older releases restarted at one response's count.
/// The parser gives both, each under its own identity, and the store counts
/// each session by one of them.
pub const CODEX_RESPONSES_FORMAT: &str = "codex-thread-usage-v1";

/// Claude Code releases whose transcripts were checked against a fixture.
const CLAUDE_CHECKED: [&str; 21] = [
    "2.1.246", "2.1.247", "2.1.248", "2.1.251", "2.1.259", "2.1.260", "2.1.261", "2.1.263",
    "2.1.267", "2.1.268", "2.1.270", "2.1.271", "2.1.272", "2.1.273", "2.1.274", "2.1.275",
    "2.1.276", "2.1.277", "2.1.278", "2.1.280", "2.1.287",
];

// Keep the initially accepted forward-compatibility floors fixed when adding
// checked fixtures, so those additions cannot revoke already counted releases.
const CODEX_FORWARD_COMPATIBLE_AFTER: (u32, u32, u32) = (0, 160, 0);
const CLAUDE_FORWARD_COMPATIBLE_AFTER: (u32, u32, u32) = (2, 1, 280);

/// `major.minor.patch`, and nothing else: a pre-release or a build suffix
/// is not a release this reads as one.
fn release(version: &str) -> Option<(u32, u32, u32)> {
    let mut parts = version.split('.');
    let mut next = || parts.next()?.parse::<u32>().ok();
    let release = (next()?, next()?, next()?);
    parts.next().is_none().then_some(release)
}

/// Whether a release is checked, or newer within the same major version than
/// the established compatibility floor. Adding checked fixtures must not raise
/// that floor and stop counting releases previously admitted by the parser.
fn counted(version: &str, checked: &[&str], floor: (u32, u32, u32)) -> bool {
    if checked.contains(&version) {
        return true;
    }
    release(version).is_some_and(|this| this.0 == floor.0 && this > floor)
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
                // Fixtures establish these local formats; a later release is
                // read with the newest checked one's semantics (`counted`).
                if !counted(&version, &CODEX_CHECKED, CODEX_FORWARD_COMPATIBLE_AFTER) {
                    return Err("unsupported Codex rollout version".into());
                }
                if self.session.as_ref().is_some_and(|old| old != &session) {
                    return Err("Codex rollout changed session identity".into());
                }
                self.session = Some(session);
                self.version = Some(version);
                self.provider = label(&payload["model_provider"]);
                self.from_start = at_file_start && payload.get("forked_from_id").is_none();
                self.parent = agentdocker_core::codex_thread::session_meta(record)
                    .and_then(|meta| meta.parent_thread);
                Ok(None)
            }
            Some("token_usage_record") => {
                if self.version.is_none() {
                    return Err("Codex usage has no supported format context".into());
                }
                let usage = agentdocker_core::codex_thread::usage_record(record)
                    .ok_or("Codex thread usage record is malformed")?;
                let counters = codex_counters(usage.usage.as_ref().unwrap_or(&Value::Null))?;
                let response = usage
                    .response
                    .ok_or("Codex thread usage record has no response id")?;
                let session = usage
                    .thread
                    .clone()
                    .or_else(|| self.session.clone())
                    .ok_or("Codex usage precedes session metadata")?;
                let at = timestamp(&record["timestamp"])?;
                Ok(Some(Sample {
                    source_id: identity("codex", &session, &response),
                    runtime: "codex".into(),
                    format: CODEX_RESPONSES_FORMAT.into(),
                    session_id: session,
                    at,
                    provider: self.provider.clone(),
                    model: self.model.clone(),
                    counters,
                    semantics: Semantics::Response,
                    proves_zero_baseline: false,
                    parent_session: self.parent.clone(),
                }))
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
                    format: match self.version.as_deref() {
                        Some("0.153.4" | "0.154.0" | "0.155.1") => {
                            "codex-rollout-0.153.4-0.154.0-v1"
                        }
                        _ => "codex-rollout-0.160.0-v1",
                    }
                    .into(),
                    session_id: session.clone(),
                    at,
                    provider: self.provider.clone(),
                    model: self.model.clone(),
                    counters,
                    semantics: Semantics::Cumulative,
                    proves_zero_baseline: first,
                    parent_session: self.parent.clone(),
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
    if !record["version"]
        .as_str()
        .is_some_and(|version| counted(version, &CLAUDE_CHECKED, CLAUDE_FORWARD_COMPATIBLE_AFTER))
    {
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
        parent_session: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn observed_historical_claude_patches_preserve_accounting_identity_and_totals() {
        // Only numeric accounting fields from observed records are retained;
        // session/message/model/timestamp values are synthetic. Nested cache
        // details remain descriptive, not an additional contribution.
        let records = include_str!("usage/fixtures/claude-historical-patches.jsonl");
        let expected = [
            [41258, 0, 41256, 399, 133],
            [40828, 0, 40826, 470, 140],
            [34446, 0, 34444, 146, 0],
            [34446, 0, 34444, 146, 0],
            [39286, 26354, 12930, 572, 169],
            [40240, 26445, 13793, 354, 120],
            [41795, 26445, 15348, 509, 346],
            [692513, 26443, 666068, 2299, 1607],
            [17415, 0, 17413, 4, 0],
        ];
        assert_eq!(records.lines().count(), expected.len());
        for (line, counters) in records.lines().zip(expected) {
            let mut record: Value = serde_json::from_str(line).unwrap();
            let parsed = claude(&record).unwrap().unwrap();
            assert_eq!(parsed.counters.values(), counters.map(Some));
            record["version"] = json!("2.1.268");
            assert_eq!(claude(&record).unwrap().unwrap(), parsed);
            record["version"] = json!("2.1.249");
            assert!(
                claude(&record).is_err(),
                "unobserved versions remain unknown"
            );
        }
    }

    #[test]
    fn observed_claude_patch_records_keep_top_level_accounting_authoritative() {
        // Accounting-only records from installed 2.1.277/278/280/287 transcripts.
        // Identifiers, timestamps and model names are synthetic; no content is
        // retained. Nested cache/iteration totals must not be counted again,
        // including a 2.1.278 record whose top-level counters are all zero.
        let records = include_str!("usage/fixtures/claude-current-patches.jsonl");
        let expected = [
            [Some(19166), Some(0), Some(19164), Some(150), Some(65)],
            [Some(41759), Some(24649), Some(17108), Some(276), Some(61)],
            [Some(0), Some(0), Some(0), Some(0), Some(0)],
            [Some(31131), Some(0), Some(31129), Some(6), None],
            [Some(35138), Some(17227), Some(17909), Some(8), None],
            [Some(44729), Some(43498), Some(1229), Some(120), Some(0)],
        ];
        assert_eq!(records.lines().count(), expected.len());
        for (line, expected) in records.lines().zip(expected) {
            let mut record: Value = serde_json::from_str(line).unwrap();
            let parsed = claude(&record).unwrap().unwrap();
            assert_eq!(parsed.counters.values(), expected);
            record["version"] = json!("2.1.276");
            assert_eq!(parsed, claude(&record).unwrap().unwrap());
            record["version"] = json!("2.1.279");
            assert!(claude(&record).is_err(), "unobserved patches stay unknown");
            // A release after the newest checked counts as it reports.
            record["version"] = json!("2.1.292");
            assert_eq!(parsed, claude(&record).unwrap().unwrap());
        }
    }

    /// Checked releases count; so does a later release of the same major
    /// version than the established floor, since both providers ship every few
    /// days. An older unchecked release, another major version, a
    /// pre-release or something that is not a version stays refused.
    #[test]
    fn later_checked_fixtures_preserve_the_previously_counted_release_range() {
        for version in [
            "2.1.280", "2.1.281", "2.1.282", "2.1.283", "2.1.284", "2.1.285", "2.1.286", "2.1.287",
            "2.1.292", "2.2.0", "2.10.0",
        ] {
            assert!(
                counted(version, &CLAUDE_CHECKED, CLAUDE_FORWARD_COMPATIBLE_AFTER),
                "{version}"
            );
        }
        for version in [
            "2.1.279",
            "2.1.200",
            "1.9.9",
            "3.0.0",
            "2.1.300-beta",
            "2.1",
            "2.1.x",
            "future",
            "",
        ] {
            assert!(
                !counted(version, &CLAUDE_CHECKED, CLAUDE_FORWARD_COMPATIBLE_AFTER),
                "{version}"
            );
        }
        for version in ["0.160.0", "0.160.1", "0.161.0", "0.200.3"] {
            assert!(
                counted(version, &CODEX_CHECKED, CODEX_FORWARD_COMPATIBLE_AFTER),
                "{version}"
            );
        }
        for version in ["0.159.9", "0.152.0", "1.0.0", "0.160.1-alpha.2"] {
            assert!(
                !counted(version, &CODEX_CHECKED, CODEX_FORWARD_COMPATIBLE_AFTER),
                "{version}"
            );
        }
    }

    #[test]
    fn observed_codex_patch_records_keep_cumulative_semantics_after_resume() {
        let records: Vec<Value> = include_str!("usage/fixtures/codex-0.155.1.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let mut parser = Codex::default();
        assert!(parser.feed(&records[0], true).unwrap().is_none());
        let expected = [
            [Some(15956), Some(13056), Some(0), Some(177), Some(0)],
            [Some(32192), Some(28800), Some(0), Some(252), Some(0)],
            [Some(48625), Some(44800), Some(0), Some(353), Some(0)],
        ];
        for (index, (record, expected)) in records[1..].iter().zip(expected).enumerate() {
            let parsed = parser.feed(record, false).unwrap().unwrap();
            assert_eq!(parsed.counters.values(), expected);
            assert_eq!(parsed.semantics, Semantics::Cumulative);
            assert_eq!(parsed.proves_zero_baseline, index == 0);
            // A persisted cursor resumes the same family; totals are not
            // mistaken for last-turn deltas or a second zero baseline.
            parser = serde_json::from_str(&serde_json::to_string(&parser).unwrap()).unwrap();
        }
    }

    #[test]
    fn observed_codex_0160_records_keep_totals_and_deduplicate_after_cursor_resume() {
        let records: Vec<Value> = include_str!("usage/fixtures/codex-0.160.0.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let mut parser = Codex::default();
        assert!(parser.feed(&records[0], true).unwrap().is_none());
        let expected = [
            [Some(5), Some(0), Some(0), Some(2), Some(0)],
            [Some(10), Some(0), Some(0), Some(4), Some(0)],
        ];
        for (index, (record, counts)) in records[1..].iter().zip(expected).enumerate() {
            let sample = parser.feed(record, false).unwrap().unwrap();
            assert_eq!(sample.format, "codex-rollout-0.160.0-v1");
            assert_eq!(sample.counters.values(), counts);
            assert_eq!(sample.semantics, Semantics::Cumulative);
            assert_eq!(sample.proves_zero_baseline, index == 0);
            parser = serde_json::from_str(&serde_json::to_string(&parser).unwrap()).unwrap();
            let repeated = parser.feed(record, false).unwrap().unwrap();
            assert_eq!(repeated.source_id, sample.source_id);
            assert!(!repeated.proves_zero_baseline);
        }
    }

    #[test]
    fn observed_codex_0164_records_give_both_accounts_and_name_the_parent_thread() {
        let records: Vec<Value> = include_str!("usage/fixtures/codex-0.164.0.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let mut parser = Codex::default();
        assert!(parser.feed(&records[0], true).unwrap().is_none());
        assert!(parser.feed(&records[1], false).unwrap().is_none());
        let mut samples = Vec::new();
        for record in &records[2..] {
            samples.push(parser.feed(record, false).unwrap().unwrap());
            // The parent survives the cursor's round trip.
            parser = serde_json::from_str(&serde_json::to_string(&parser).unwrap()).unwrap();
        }
        // Each turn: the response's own count, then the running totals.
        let responses = [
            [Some(5), Some(0), Some(0), Some(2), Some(0)],
            [Some(9), Some(4), Some(0), Some(3), Some(1)],
        ];
        let totals = [
            [Some(5), Some(0), Some(0), Some(2), Some(0)],
            [Some(14), Some(4), Some(0), Some(5), Some(1)],
        ];
        assert_eq!(samples.len(), 4);
        for (turn, pair) in samples.chunks(2).enumerate() {
            let (response, total) = (&pair[0], &pair[1]);
            assert_eq!(response.format, CODEX_RESPONSES_FORMAT);
            assert_eq!(response.counters.values(), responses[turn]);
            assert_eq!(response.semantics, Semantics::Response);
            assert!(!response.proves_zero_baseline);
            assert_eq!(total.format, "codex-rollout-0.160.0-v1");
            assert_eq!(total.counters.values(), totals[turn]);
            assert_eq!(total.semantics, Semantics::Cumulative);
            assert_eq!(total.proves_zero_baseline, turn == 0);
            for sample in pair {
                assert_eq!(sample.session_id, "fixture-child-thread");
                assert_eq!(
                    sample.parent_session.as_deref(),
                    Some("fixture-root-thread")
                );
                assert_eq!(sample.model.as_deref(), Some("fixture-model"));
            }
            assert_ne!(response.source_id, total.source_id);
        }
        assert_ne!(samples[0].source_id, samples[2].source_id);
        let repeated = parser.feed(&records[2], false).unwrap().unwrap();
        assert_eq!(repeated.source_id, samples[0].source_id);
        // A rollout no thread spawned names no parent, and its sample's
        // identity is the one older builds gave it.
        let mut root = Codex::default();
        root.feed(&json!({"type":"session_meta","payload":{"id":"thread-a","cli_version":"0.164.0","source":"cli"}}), true).unwrap();
        let sample = root.feed(&records[2], false).unwrap().unwrap();
        assert_eq!(sample.parent_session, None);
        assert!(
            !serde_json::to_string(&sample)
                .unwrap()
                .contains("parent_session")
        );
    }

    #[test]
    fn codex_response_records_need_their_context_and_ids() {
        let usage = json!({"input_tokens":5,"output_tokens":2});
        let record = |payload: Value| json!({"type":"token_usage_record","timestamp":"2026-10-20T09:00:04Z","payload":payload});
        let mut parser = Codex::default();
        assert!(
            parser
                .feed(&record(json!({"response_id":"r","usage":usage})), false)
                .is_err()
        );
        parser
            .feed(
                &json!({"type":"session_meta","payload":{"id":"thread-a","cli_version":"0.164.0"}}),
                true,
            )
            .unwrap();
        assert!(parser.feed(&record(json!({"usage":usage})), false).is_err());
        assert!(
            parser
                .feed(&record(json!({"response_id":"r"})), false)
                .is_err()
        );
        assert!(
            parser
                .feed(
                    &record(json!({"response_id":"r","usage":{"input_tokens":-1}})),
                    false
                )
                .is_err()
        );
        let sample = parser
            .feed(&record(json!({"response_id":"r","usage":usage})), false)
            .unwrap()
            .unwrap();
        assert_eq!(sample.session_id, "thread-a");
        assert_eq!(sample.counters.input_tokens, Some(5));
        assert_eq!(sample.counters.cache_read_input_tokens, None);
    }

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
        for version in ["0.153.4", "0.154.0", "0.155.1", "0.160.0", "0.164.0"] {
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
        // A later release reads with the newest checked one's format.
        let mut later = Codex::default();
        later
            .feed(
                &json!({"type":"session_meta","payload":{"id":"thread-a","cli_version":"0.160.1"}}),
                true,
            )
            .unwrap();
        assert_eq!(
            later.feed(&record, false).unwrap().unwrap().format,
            "codex-rollout-0.160.0-v1"
        );
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
            "2.1.277", "2.1.278", "2.1.280",
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
