//! Claude Code's OpenTelemetry events as usage samples.
//!
//! Claude Code exports its events as OTLP log records (`OTEL_LOGS_EXPORTER=otlp`);
//! this module reads the OTLP/HTTP JSON encoding of an `ExportLogsServiceRequest`
//! and keeps one thing from it: each `api_request` event, which carries the
//! tokens of one API response and what the provider charged for it. Nothing
//! else in the payload is kept — not prompts, tool arguments or any other
//! event — and no byte of the payload persists past this decoding.
//!
//! Attribute names follow the documented contract
//! (<https://code.claude.com/docs/en/monitoring-usage>): `event.name`,
//! `session.id`, `request_id`, `model`, `input_tokens`, `output_tokens`,
//! `cache_read_tokens`, `cache_creation_tokens`, `cost_usd` and
//! `cost_usd_micros`, `event.timestamp`.

use super::{Sample, Semantics, request_identity};
use agentdocker_core::usage::Counters;
use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

/// The accounting format telemetry samples carry, so a report can say
/// which samples came by this route.
pub const FORMAT: &str = "claude-otlp-events-v1";

/// The runtime telemetry events belong to: the same as its transcripts,
/// so the two sources share one session and one dedupe space.
pub const RUNTIME: &str = "claude-code";

/// The event that is a usage sample, by its `event.name` attribute, with
/// and without the `claude_code.` prefix the log body carries.
const API_REQUEST: &str = "api_request";

/// The most log records one payload may hold before it is refused whole:
/// the exporter batches a few seconds of events, never thousands.
pub const MAX_RECORDS: usize = 4_096;

/// What one payload held, before anything is stored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decoded {
    /// One per `api_request` event that named its session and request.
    pub samples: Vec<Sample>,
    /// Log records in the payload, of every kind.
    pub records: u64,
    /// `api_request` events left out, each with why; a reader reports the
    /// count, never the event.
    pub skipped: Vec<String>,
}

/// Read an OTLP/JSON logs export. A payload that is not one is refused;
/// an `api_request` event the reader cannot account (no session, no
/// request id, no counter) is skipped with its reason, so one odd event
/// never loses the batch around it.
pub fn decode_logs(payload: &Value) -> Result<Decoded, String> {
    let resources = payload
        .get("resourceLogs")
        .and_then(Value::as_array)
        .ok_or("not an OTLP logs export: no resourceLogs array")?;
    let mut decoded = Decoded::default();
    for resource in resources {
        let scopes = resource
            .get("scopeLogs")
            .and_then(Value::as_array)
            .ok_or("not an OTLP logs export: resourceLogs without scopeLogs")?;
        for scope in scopes {
            let records = scope
                .get("logRecords")
                .and_then(Value::as_array)
                .ok_or("not an OTLP logs export: scopeLogs without logRecords")?;
            for record in records {
                decoded.records += 1;
                if decoded.records as usize > MAX_RECORDS {
                    return Err(format!("more than {MAX_RECORDS} log records in one export"));
                }
                let attributes = Attributes::of(record);
                if !attributes.is_event(API_REQUEST) {
                    continue;
                }
                match sample(record, &attributes) {
                    Ok(sample) => decoded.samples.push(sample),
                    Err(reason) => decoded.skipped.push(reason),
                }
            }
        }
    }
    Ok(decoded)
}

/// How many data points an OTLP/JSON metrics export carries. The daemon
/// acknowledges metrics so the exporter does not retry them, and reads
/// nothing from them: the per-request events are the accounting, and the
/// cumulative counters would count the same tokens again.
pub fn count_metrics(payload: &Value) -> Result<u64, String> {
    let resources = payload
        .get("resourceMetrics")
        .and_then(Value::as_array)
        .ok_or("not an OTLP metrics export: no resourceMetrics array")?;
    let mut points = 0;
    for resource in resources {
        for scope in resource
            .get("scopeMetrics")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            for metric in scope
                .get("metrics")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                for kind in ["sum", "gauge", "histogram"] {
                    points += metric
                        .get(kind)
                        .and_then(|k| k.get("dataPoints"))
                        .and_then(Value::as_array)
                        .map_or(0, |p| p.len() as u64);
                }
            }
        }
    }
    Ok(points)
}

fn sample(record: &Value, attributes: &Attributes<'_>) -> Result<Sample, String> {
    let session = attributes
        .string("session.id")
        .ok_or("api_request without session.id")?;
    let request = attributes
        .string("request_id")
        .ok_or("api_request without request_id")?;
    let uncached = attributes.integer("input_tokens")?;
    let read = attributes.integer("cache_read_tokens")?;
    let write = attributes.integer("cache_creation_tokens")?;
    // Claude's input_tokens excludes cached reads and writes, as in its
    // transcripts: the total input is the three together, once.
    let input = match (uncached, read, write) {
        (Some(a), Some(b), Some(c)) => Some(
            a.checked_add(b)
                .and_then(|n| n.checked_add(c))
                .ok_or("api_request total input overflows")?,
        ),
        _ => None,
    };
    let counters = super::consistent(Counters {
        input_tokens: input,
        cache_read_input_tokens: read,
        cache_write_input_tokens: write,
        output_tokens: attributes.integer("output_tokens")?,
        reasoning_output_tokens: None,
    })?;
    let cost_usd_micros = match attributes.integer("cost_usd_micros")? {
        Some(micros) => Some(micros),
        None => attributes.cost_usd()?,
    };
    let at = match attributes.string("event.timestamp") {
        // Full precision: a card is charged for a response made after it
        // was taken, and a pull and a response can share a second.
        Some(text) => DateTime::parse_from_rfc3339(&text)
            .map(|at| at.with_timezone(&Utc))
            .map_err(|_| "api_request event.timestamp is not RFC 3339".to_owned())?,
        None => time_unix_nano(record)?,
    };
    Ok(Sample {
        source_id: request_identity(RUNTIME, &session, &request),
        runtime: RUNTIME.to_owned(),
        format: FORMAT.to_owned(),
        session_id: session,
        at,
        provider: None,
        model: attributes.string("model"),
        counters,
        semantics: Semantics::Response,
        proves_zero_baseline: false,
        cost_usd_micros,
        aliases: Vec::new(),
    })
}

/// The record's own time, nanoseconds since the epoch as a string or a
/// number (OTLP/JSON writes 64-bit integers as strings).
fn time_unix_nano(record: &Value) -> Result<DateTime<Utc>, String> {
    // Zero is OTLP's "unknown"; the time the exporter observed the record
    // stands in for it.
    let nanos = ["timeUnixNano", "observedTimeUnixNano"]
        .into_iter()
        .filter_map(|key| record.get(key).and_then(any_integer))
        .find(|nanos| *nanos > 0)
        .ok_or("api_request has neither event.timestamp nor timeUnixNano")?;
    let nanos = i64::try_from(nanos).map_err(|_| "api_request time is out of range".to_owned())?;
    Ok(Utc.timestamp_nanos(nanos))
}

/// A record's attributes by key, as OTLP/JSON spells them: a list of
/// `{key, value: {stringValue | intValue | doubleValue | …}}`.
struct Attributes<'a> {
    pairs: Vec<(&'a str, &'a Value)>,
    body: Option<&'a str>,
}

impl<'a> Attributes<'a> {
    fn of(record: &'a Value) -> Self {
        let pairs = record
            .get("attributes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|pair| Some((pair.get("key")?.as_str()?, pair.get("value")?)))
            .collect();
        let body = record
            .get("body")
            .and_then(|body| body.get("stringValue"))
            .and_then(Value::as_str);
        Self { pairs, body }
    }

    fn value(&self, key: &str) -> Option<&'a Value> {
        self.pairs
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, value)| *value)
    }

    /// Whether this record is the named event: its `event.name` says so,
    /// with or without the `claude_code.` prefix, or its body does when
    /// the attribute is missing.
    fn is_event(&self, name: &str) -> bool {
        let prefixed = format!("claude_code.{name}");
        match self.string("event.name") {
            Some(event) => event == name || event == prefixed,
            None => self.body == Some(prefixed.as_str()),
        }
    }

    fn string(&self, key: &str) -> Option<String> {
        self.value(key)?
            .get("stringValue")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
            .map(str::to_owned)
    }

    /// A nonnegative integer attribute, however the exporter spelt it:
    /// `intValue` as a string or number, or a whole `doubleValue`.
    fn integer(&self, key: &str) -> Result<Option<u64>, String> {
        let Some(value) = self.value(key) else {
            return Ok(None);
        };
        let number = value
            .get("intValue")
            .or_else(|| value.get("doubleValue"))
            .or_else(|| value.get("stringValue"))
            .ok_or_else(|| format!("api_request {key} is not a number"))?;
        any_integer(number)
            .map(Some)
            .ok_or_else(|| format!("api_request {key} is not a nonnegative integer"))
    }

    /// `cost_usd` as micro-dollars, rounded to the nearest; a price that
    /// is not a nonnegative finite number is a refused event, never a
    /// zero one.
    fn cost_usd(&self) -> Result<Option<u64>, String> {
        let Some(value) = self.value("cost_usd") else {
            return Ok(None);
        };
        let dollars = value
            .get("doubleValue")
            .or_else(|| value.get("intValue"))
            .or_else(|| value.get("stringValue"))
            .and_then(|v| match v {
                Value::Number(n) => n.as_f64(),
                Value::String(s) => s.parse().ok(),
                _ => None,
            })
            .filter(|d| d.is_finite() && *d >= 0.0 && *d <= 1e9)
            .ok_or("api_request cost_usd is not a nonnegative number")?;
        Ok(Some((dollars * 1e6).round() as u64))
    }
}

/// A whole nonnegative number from a JSON number or the string OTLP/JSON
/// uses for 64-bit integers.
fn any_integer(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64().or_else(|| {
            n.as_f64()
                .filter(|f| f.fract() == 0.0 && *f >= 0.0)
                .map(|f| f as u64)
        }),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The payload Claude Code's exporter sends, as the OTLP/JSON encoding
    /// spells it: integers as strings, the price as a double, the event
    /// named in both the body and `event.name`, other events beside it.
    fn export() -> Value {
        json!({"resourceLogs":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"claude-code"}}]},
        "scopeLogs":[{"scope":{"name":"com.anthropic.claude_code.events"},"logRecords":[
            {"timeUnixNano":"1766000000000000000","severityNumber":9,"body":{"stringValue":"claude_code.api_request"},
             "attributes":[
                {"key":"event.name","value":{"stringValue":"api_request"}},
                {"key":"event.timestamp","value":{"stringValue":"2026-10-25T10:15:30.250Z"}},
                {"key":"session.id","value":{"stringValue":"sess-1"}},
                {"key":"user.id","value":{"stringValue":"anonymous-hash"}},
                {"key":"prompt.id","value":{"stringValue":"prompt-7"}},
                {"key":"request_id","value":{"stringValue":"req_fixture_1"}},
                {"key":"model","value":{"stringValue":"fixture-model"}},
                {"key":"cost_usd","value":{"doubleValue":0.0123456}},
                {"key":"duration_ms","value":{"intValue":"812"}},
                {"key":"input_tokens","value":{"intValue":"12"}},
                {"key":"output_tokens","value":{"intValue":"340"}},
                {"key":"cache_read_tokens","value":{"intValue":"24000"}},
                {"key":"cache_creation_tokens","value":{"intValue":"1500"}}]},
            {"timeUnixNano":"1766000001000000000","body":{"stringValue":"claude_code.tool_decision"},
             "attributes":[{"key":"event.name","value":{"stringValue":"tool_decision"}},
                {"key":"session.id","value":{"stringValue":"sess-1"}},
                {"key":"tool_name","value":{"stringValue":"Bash"}},
                {"key":"decision","value":{"stringValue":"accept"}}]},
            {"timeUnixNano":"1766000002000000000","body":{"stringValue":"claude_code.api_request"},
             "attributes":[{"key":"event.name","value":{"stringValue":"claude_code.api_request"}},
                {"key":"session.id","value":{"stringValue":"sess-1"}},
                {"key":"request_id","value":{"stringValue":"req_second"}},
                {"key":"cost_usd_micros","value":{"intValue":2500}},
                {"key":"input_tokens","value":{"intValue":5}},
                {"key":"output_tokens","value":{"intValue":7}},
                {"key":"cache_read_tokens","value":{"intValue":0}},
                {"key":"cache_creation_tokens","value":{"intValue":0}}]},
            {"timeUnixNano":"1766000003000000000","body":{"stringValue":"claude_code.api_request"},
             "attributes":[{"key":"event.name","value":{"stringValue":"api_request"}},
                {"key":"session.id","value":{"stringValue":"sess-1"}},
                {"key":"input_tokens","value":{"intValue":"1"}}]}
        ]}]}]})
    }

    #[test]
    fn api_request_events_become_priced_samples_and_nothing_else_is_kept() {
        let decoded = decode_logs(&export()).unwrap();
        assert_eq!(decoded.records, 4);
        assert_eq!(decoded.samples.len(), 2);
        assert_eq!(decoded.skipped, ["api_request without request_id"]);
        let first = &decoded.samples[0];
        assert_eq!(first.runtime, RUNTIME);
        assert_eq!(first.format, FORMAT);
        assert_eq!(first.session_id, "sess-1");
        assert_eq!(first.model.as_deref(), Some("fixture-model"));
        assert_eq!(first.cost_usd_micros, Some(12_346));
        assert_eq!(
            first.counters.values(),
            [Some(25_512), Some(24_000), Some(1_500), Some(340), None]
        );
        assert_eq!(first.at.to_rfc3339(), "2026-10-25T10:15:30.250+00:00");
        assert_eq!(first.semantics, Semantics::Response);
        assert_eq!(
            first.source_id,
            request_identity("claude-code", "sess-1", "req_fixture_1"),
            "the telemetry sample is the transcript record's alias"
        );
        assert_eq!(
            first.source_id,
            super::super::claude(&json!({"type":"assistant","version":"2.1.287","sessionId":"sess-1",
                "requestId":"req_fixture_1","timestamp":"2026-10-25T10:15:30Z",
                "message":{"id":"msg_1","model":"fixture-model","usage":{"input_tokens":12,
                "cache_read_input_tokens":24000,"cache_creation_input_tokens":1500,"output_tokens":340}}}))
            .unwrap()
            .unwrap()
            .aliases[0]
        );
        let second = &decoded.samples[1];
        assert_eq!(
            second.cost_usd_micros,
            Some(2_500),
            "micros win over nothing"
        );
        assert_eq!(second.at.to_rfc3339(), "2025-12-17T19:33:22+00:00");
        assert_eq!(second.counters.input_tokens, Some(5));
        let text = serde_json::to_string(&decoded.samples).unwrap();
        assert!(!text.contains("prompt-7") && !text.contains("anonymous-hash"));
    }

    #[test]
    fn payloads_that_are_not_logs_exports_are_refused_whole() {
        for bad in [
            json!({}),
            json!({"resourceLogs": "x"}),
            json!({"resourceLogs": [{"scopeLogs": [{"logRecords": "x"}]}]}),
            json!({"resourceMetrics": []}),
        ] {
            assert!(decode_logs(&bad).is_err(), "{bad}");
        }
        assert_eq!(
            decode_logs(&json!({"resourceLogs": []})).unwrap(),
            Decoded::default()
        );
        let mut many = export();
        let records = &mut many["resourceLogs"][0]["scopeLogs"][0]["logRecords"];
        let one = records[1].clone();
        for _ in 0..MAX_RECORDS {
            records.as_array_mut().unwrap().push(one.clone());
        }
        assert!(decode_logs(&many).is_err());
    }

    #[test]
    fn odd_prices_and_counters_skip_the_event_rather_than_invent_a_value() {
        let event = |extra: Value| {
            let mut attributes = vec![
                json!({"key":"event.name","value":{"stringValue":"api_request"}}),
                json!({"key":"session.id","value":{"stringValue":"s"}}),
                json!({"key":"request_id","value":{"stringValue":"r"}}),
                json!({"key":"input_tokens","value":{"intValue":"1"}}),
                json!({"key":"output_tokens","value":{"intValue":"1"}}),
            ];
            attributes.extend(extra.as_array().cloned().unwrap_or_default());
            json!({"resourceLogs":[{"scopeLogs":[{"logRecords":[{"timeUnixNano":"1766000000000000000","attributes":attributes}]}]}]})
        };
        let priced = decode_logs(&event(
            json!([{"key":"cost_usd","value":{"doubleValue":0.5}}]),
        ))
        .unwrap();
        assert_eq!(priced.samples[0].cost_usd_micros, Some(500_000));
        assert_eq!(
            priced.samples[0].counters.input_tokens, None,
            "no cache counters, no total"
        );
        for bad in [
            json!([{"key":"cost_usd","value":{"doubleValue":-0.5}}]),
            json!([{"key":"cost_usd","value":{"stringValue":"free"}}]),
            json!([{"key":"cache_creation_tokens","value":{"doubleValue":1.5}}]),
            json!([{"key":"cache_read_tokens","value":{"intValue":"-3"}}]),
        ] {
            let decoded = decode_logs(&event(bad.clone())).unwrap();
            assert!(decoded.samples.is_empty(), "{bad}");
            assert_eq!(decoded.skipped.len(), 1);
        }
        let unpriced = decode_logs(&event(json!([]))).unwrap();
        assert_eq!(unpriced.samples[0].cost_usd_micros, None);
        let no_time = json!({"resourceLogs":[{"scopeLogs":[{"logRecords":[{"attributes":[
            {"key":"event.name","value":{"stringValue":"api_request"}},
            {"key":"session.id","value":{"stringValue":"s"}},
            {"key":"request_id","value":{"stringValue":"r"}},
            {"key":"output_tokens","value":{"intValue":"1"}}]}]}]}]});
        assert_eq!(
            decode_logs(&no_time).unwrap().skipped,
            ["api_request has neither event.timestamp nor timeUnixNano"]
        );
        let mut observed = no_time.clone();
        let record = &mut observed["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0];
        record["timeUnixNano"] = json!("0");
        record["observedTimeUnixNano"] = json!("1766000000123456789");
        let sample = &decode_logs(&observed).unwrap().samples[0];
        assert_eq!(
            sample.at.timestamp_nanos_opt(),
            Some(1_766_000_000_123_456_789)
        );
    }

    #[test]
    fn metrics_exports_are_counted_and_nothing_is_read_from_them() {
        let metrics = json!({"resourceMetrics":[{"scopeMetrics":[{"metrics":[
            {"name":"claude_code.token.usage","sum":{"dataPoints":[{"asInt":"5"},{"asInt":"7"}]}},
            {"name":"claude_code.cost.usage","sum":{"dataPoints":[{"asDouble":0.1}]}}]}]}]});
        assert_eq!(count_metrics(&metrics), Ok(3));
        assert!(count_metrics(&json!({"resourceLogs": []})).is_err());
    }
}
