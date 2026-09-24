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
