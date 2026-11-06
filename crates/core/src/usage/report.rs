//! Usage query and response values. Coverage belongs beside every total.

use super::{Aggregate, CounterReport, Coverage};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    #[default]
    Agent,
    Model,
    Provider,
    Project,
    Hour,
    /// The board card (`task:<id>` lease) the agent held when the sample
    /// was recorded; null for samples made while it held none.
    Card,
    /// The agent's roles when the sample was recorded, as one label
    /// (`reviewer`, or `reviewer,tester` for several); null for none.
    Role,
}

impl Group {
    pub const ALL: [Group; 7] = [
        Group::Agent,
        Group::Model,
        Group::Provider,
        Group::Project,
        Group::Hour,
        Group::Card,
        Group::Role,
    ];

    /// The wire and command-line name: `agent`, `model`, … `card`, `role`.
    pub fn as_str(self) -> &'static str {
        match self {
            Group::Agent => "agent",
            Group::Model => "model",
            Group::Provider => "provider",
            Group::Project => "project",
            Group::Hour => "hour",
            Group::Card => "card",
            Group::Role => "role",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|group| group.as_str() == text)
    }
}

/// References are resolved by the daemon before querying historical buckets.
/// `since` accepts RFC3339 or the strict duration grammar in `usage::since`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub by: Group,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterReports {
    pub input_tokens: CounterReport,
    pub cache_read_input_tokens: CounterReport,
    pub cache_write_input_tokens: CounterReport,
    pub output_tokens: CounterReport,
    pub reasoning_output_tokens: CounterReport,
}

impl CounterReports {
    pub fn new(aggregate: &Aggregate, complete: bool) -> Self {
        let [
            input_tokens,
            cache_read_input_tokens,
            cache_write_input_tokens,
            output_tokens,
            reasoning_output_tokens,
        ] = aggregate.counters(complete);
        Self {
            input_tokens,
            cache_read_input_tokens,
            cache_write_input_tokens,
            output_tokens,
            reasoning_output_tokens,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    /// Null is unattributed/unknown, never an invented provider or agent.
    pub key: Option<String>,
    pub samples: u64,
    pub counters: CounterReports,
    /// The provider-reported estimated cost of the samples, in micro-dollars (USD ×
    /// 10⁶), with the same coverage terms as a counter: a sum only over the
    /// samples that carried a price (Claude Code's telemetry events do;
    /// transcripts do not). Unknown on an older daemon's report.
    #[serde(default)]
    pub cost_usd_micros: CounterReport,
    /// Of the input tokens the samples reported, the share read from the
    /// cache, in whole percent; null when either count is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_share_percent: Option<u8>,
}

impl Row {
    /// The cache-read share this row's counters give: cache reads over
    /// total input (both totals include their cache components), clamped
    /// to 100 against a source that reports otherwise. None when either
    /// sum is unknown, when the two sums do not cover the same samples (a
    /// record that reports its cache read without its cache write has no
    /// total input but a known read, so the read would be counted over an
    /// input that leaves it out), or no input was reported at all.
    pub fn cache_read_share(counters: &CounterReports) -> Option<u8> {
        if counters.input_tokens.known_samples != counters.cache_read_input_tokens.known_samples {
            return None;
        }
        let input = counters.input_tokens.sum?;
        let read = counters.cache_read_input_tokens.sum?;
        if input == 0 {
            return None;
        }
        // In u128: a hundred times a large u64 count does not fit a u64.
        Some((u128::from(read.min(input)) * 100 / u128::from(input)) as u8)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionState {
    #[default]
    Unknown,
    Scanning,
    CaughtUp,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub roots: Vec<String>,
    pub formats: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    /// Current configuration, supplied by the daemon at query time. Older
    /// reports without this field do not establish whether collection is off.
    #[serde(default)]
    pub enabled: Option<bool>,
    pub state: CollectionState,
    pub discovery_generation: Option<u64>,
    pub snapshot_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub discovery_complete: bool,
    pub pending_files: Option<u64>,
    pub pending_tail_files: Option<u64>,
    pub scope: Scope,
    /// Where the daemon is taking Claude Code's OpenTelemetry events: the
    /// address its receiver is listening on, absent when none is (off,
    /// not yet bound, or a daemon that predates it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_listen: Option<String>,
    /// Why a configured receiver is not listening (an address another
    /// program holds, say); absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportCoverage {
    pub retained_since: DateTime<Utc>,
    pub history_truncated: bool,
    pub future_until_clamped: bool,
    pub includes_current_hour: bool,
    pub source_gaps: u64,
    pub collection: Collection,
    /// Absent on older daemons. Logical accounting bytes, not SQLite file size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking: Option<Tracking>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tracking {
    pub logical_bytes: u64,
    pub capacity_bytes: u64,
    /// This query's range overlaps records refused by tracking capacity.
    pub capacity_gap: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Estimate {
    pub value: u64,
    pub algorithm: String,
    pub version: String,
    pub parameters: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overhead {
    pub injected_bytes: Option<u64>,
    pub known_events: u64,
    pub coverage: Coverage,
    pub estimated_tokens: Option<Estimate>,
}

impl Default for Overhead {
    fn default() -> Self {
        Self {
            injected_bytes: None,
            known_events: 0,
            coverage: Coverage::Unknown,
            estimated_tokens: None,
        }
    }
}

impl Overhead {
    /// Counts from best-effort instrumentation must not look exhaustive.
    pub fn coverage_suffix(&self) -> &'static str {
        if self.injected_bytes.is_none() && self.estimated_tokens.is_none() {
            return "";
        }
        match self.coverage {
            Coverage::Complete => "",
            Coverage::Partial => "; partial coverage, recorded writes only",
            Coverage::Unknown => "; instrumentation coverage unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub rows: Vec<Row>,
    pub by: Group,
    pub as_of: DateTime<Utc>,
    pub effective_since: DateTime<Utc>,
    pub effective_until: DateTime<Utc>,
    pub coverage: ReportCoverage,
    pub overhead: Overhead,
}

/// A row's tokens, input and output together: the sum its samples gave and
/// whether some did not say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tokens {
    pub sum: u64,
    pub partial: bool,
}

impl Tokens {
    /// `1.2M`, marked `~` when some samples did not say.
    pub fn label(&self) -> String {
        format!(
            "{}{}",
            super::compact(self.sum),
            if self.partial { "~" } else { "" }
        )
    }
}

impl Report {
    /// Each row's tokens by its key. A row no sample gave a count for, and
    /// unattributed usage, are left out rather than shown as zero.
    pub fn tokens(&self) -> std::collections::BTreeMap<String, Tokens> {
        self.rows
            .iter()
            .filter_map(|row| {
                let key = row.key.clone()?;
                let parts = [&row.counters.input_tokens, &row.counters.output_tokens];
                if parts.iter().all(|c| c.sum.is_none()) {
                    return None;
                }
                let sum = parts
                    .iter()
                    .filter_map(|c| c.sum)
                    .fold(0u64, u64::saturating_add);
                let partial = parts.iter().any(|c| c.coverage != Coverage::Complete);
                Some((key, Tokens { sum, partial }))
            })
            .collect()
    }

    /// Every row's tokens together, or nothing when no row has a count.
    pub fn total(&self) -> Option<Tokens> {
        let tokens = self.tokens();
        (!tokens.is_empty()).then(|| Tokens {
            sum: tokens.values().map(|t| t.sum).fold(0, u64::saturating_add),
            partial: tokens.values().any(|t| t.partial),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every group has one name, read back as itself; the two newer ones
    /// are among them, and a stranger is nobody.
    #[test]
    fn groups_round_trip_through_their_names() {
        for group in Group::ALL {
            assert_eq!(Group::parse(group.as_str()), Some(group));
            let wire = serde_json::to_value(group).unwrap();
            assert_eq!(wire, serde_json::Value::String(group.as_str().to_owned()));
        }
        assert_eq!(Group::parse("card"), Some(Group::Card));
        assert_eq!(Group::parse("role"), Some(Group::Role));
        assert_eq!(Group::parse("lease"), None);
    }

    /// The cache share is cache reads over input, whole percent, only when
    /// both are known over the same samples, never over a hundred, and
    /// absent for no input.
    #[test]
    fn cache_read_share_needs_both_sums_and_stays_within_a_hundred() {
        let counter = |sum: Option<u64>| CounterReport {
            sum,
            known_samples: sum.map_or(0, |_| 1),
            coverage: Coverage::Partial,
        };
        let counters = |input: Option<u64>, read: Option<u64>| CounterReports {
            input_tokens: counter(input),
            cache_read_input_tokens: counter(read),
            cache_write_input_tokens: counter(None),
            output_tokens: counter(None),
            reasoning_output_tokens: counter(None),
        };
        assert_eq!(
            Row::cache_read_share(&counters(Some(400), Some(300))),
            Some(75)
        );
        assert_eq!(Row::cache_read_share(&counters(Some(3), Some(1))), Some(33));
        // A read known on a sample whose total input is not: the sums
        // cover different samples and give no share.
        let mut uneven = counters(Some(400), Some(300));
        uneven.cache_read_input_tokens.known_samples = 2;
        uneven.cache_read_input_tokens.sum = Some(500);
        assert_eq!(Row::cache_read_share(&uneven), None);
        assert_eq!(
            Row::cache_read_share(&counters(Some(10), Some(20))),
            Some(100)
        );
        assert_eq!(Row::cache_read_share(&counters(Some(0), Some(0))), None);
        // Counts whose hundredfold overflows a u64 keep their share.
        assert_eq!(
            Row::cache_read_share(&counters(Some(10_u64.pow(19)), Some(10_u64.pow(18)))),
            Some(10)
        );
        assert_eq!(
            Row::cache_read_share(&counters(Some(u64::MAX), Some(u64::MAX / 2))),
            Some(49)
        );
        assert_eq!(
            Row::cache_read_share(&counters(Some(u64::MAX), Some(u64::MAX))),
            Some(100)
        );
        assert_eq!(Row::cache_read_share(&counters(None, Some(5))), None);
        assert_eq!(Row::cache_read_share(&counters(Some(5), None)), None);
        let older: Row = serde_json::from_value(serde_json::json!({
            "key": null, "samples": 1,
            "counters": serde_json::to_value(counters(Some(5), None)).unwrap()
        }))
        .unwrap();
        assert_eq!(older.cost_usd_micros, CounterReport::default());
        assert_eq!(older.cache_read_share_percent, None);
    }
}
