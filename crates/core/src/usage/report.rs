//! Usage query and response values. Coverage belongs beside every total.

use super::context::Context;
use super::price::CostReport;
use super::{Aggregate, CounterReport, Coverage};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    /// The five, in the order the counters are kept.
    pub fn each_mut(&mut self) -> [&mut CounterReport; 5] {
        [
            &mut self.input_tokens,
            &mut self.cache_read_input_tokens,
            &mut self.cache_write_input_tokens,
            &mut self.output_tokens,
            &mut self.reasoning_output_tokens,
        ]
    }

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
    /// Whether every figure in the row is the provider's own per-response
    /// count (`exact`), or at least one is a difference between its
    /// running totals (`estimated`); absent where buckets written before
    /// this was kept are in the row ([`super::UsageSource`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_source: Option<super::UsageSource>,
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
    /// An estimate: the person's own price table (`[[usage.price]]` in
    /// `agentd.toml`, off unless they fill it) applied to the tokens the
    /// row reports. Kept apart from `cost_usd_micros`, which is what the
    /// provider reported, and never added to it or shown as it; present
    /// only where a price matched some of the row's usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost: Option<CostReport>,
}

impl Row {
    /// The cache-read share this row's `counters` give over its `samples`:
    /// cache reads over total input (both totals include their cache
    /// components), clamped to 100 against a source that reports
    /// otherwise. Given only when every sample of the row reported both,
    /// so the two sums cover the same responses: a record that reports
    /// its cache read without its cache write has a known read and no
    /// total input, a Codex record may lack either, and two such records
    /// would otherwise give one counter's sum over the other's samples.
    /// None otherwise, and when no input was reported at all.
    pub fn cache_read_share(samples: u64, counters: &CounterReports) -> Option<u8> {
        if samples == 0
            || counters.input_tokens.known_samples != samples
            || counters.cache_read_input_tokens.known_samples != samples
        {
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

    /// Every token count's coverage, and the estimate's, settled again
    /// from what the samples said: complete only when `complete` and every
    /// sample said. Partial coverage is kept partial, and the estimate
    /// follows its counts, since it is computed from them.
    pub fn settle_coverage(&mut self, complete: bool) {
        for counter in self.counters.each_mut() {
            counter.coverage = if counter.known_samples == 0 {
                Coverage::Unknown
            } else if complete && counter.known_samples == self.samples {
                Coverage::Complete
            } else {
                Coverage::Partial
            };
        }
        if let Some(estimate) = &mut self.estimated_cost
            && estimate.coverage == Coverage::Complete
            && !complete
        {
            estimate.coverage = Coverage::Partial;
        }
    }

    /// No figure, the reported cost and the estimate included, claims to
    /// be complete.
    pub fn demote_complete(&mut self) {
        let estimate = self.estimated_cost.as_mut().map(|e| &mut e.coverage);
        for coverage in self
            .counters
            .each_mut()
            .into_iter()
            .map(|c| &mut c.coverage)
            .chain([&mut self.cost_usd_micros.coverage])
            .chain(estimate)
        {
            if *coverage == Coverage::Complete {
                *coverage = Coverage::Partial;
            }
        }
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
    /// The walk of the roots has ended, complete or not. Without it a
    /// walk under way and one that stopped early (a bound, a root that
    /// could not be read) are the same fields. Written only when true; a
    /// collection stored before it existed reads as a walk under way.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub discovery_ended: bool,
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

/// Where a collection stands, as one thing a person can be told.
///
/// It is derived ([`Collection::phase`]), not stored: what is stored is
/// the state, the generation, the walk's two flags and the counts of
/// files still to read, and the CLI and the app each read those their own
/// way, the same way only because each wrote the same chain of
/// conditions. Neither could tell a collection walking its roots from
/// one whose walk had stopped early, and both called it unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectionPhase {
    /// The configuration has collection switched off.
    Off,
    /// Switched on, and no pass has begun: at start, or after the roots
    /// were changed.
    Starting,
    /// A pass is walking its roots for the files to read.
    Discovering,
    /// The walk ended early: a bound was reached or a root could not be
    /// read. What was found is not all there is. It stands until the next
    /// pass.
    DiscoveryIncomplete,
    /// The walk is complete and files are being read; how many are left,
    /// when that is known.
    Reading { files: Option<u64> },
    /// Everything found has been read; when, if the collection says.
    CaughtUp { at: Option<DateTime<Utc>> },
    /// Not told: the report does not say whether collection is on (a
    /// daemon from before `enabled`) and its state does not say either.
    Unknown,
}

impl CollectionPhase {
    /// The phase in a word, the same wherever it is said.
    pub fn word(self) -> &'static str {
        match self {
            CollectionPhase::Off => "off",
            CollectionPhase::Starting => "starting",
            CollectionPhase::Discovering => "discovering",
            CollectionPhase::DiscoveryIncomplete => "discovery incomplete",
            CollectionPhase::Reading { .. } => "reading",
            CollectionPhase::CaughtUp { .. } => "caught up",
            CollectionPhase::Unknown => "unknown",
        }
    }

    /// What the phase means for the totals, where its word does not say:
    /// the same sentence in the CLI and the app.
    pub fn note(self) -> Option<&'static str> {
        match self {
            CollectionPhase::DiscoveryIncomplete => Some(
                "The walk of the roots stopped early, so some files may not be counted. The next pass walks them again.",
            ),
            _ => None,
        }
    }
}

impl Collection {
    /// Where the collection stands. The one reading of its fields, for
    /// every client.
    pub fn phase(&self) -> CollectionPhase {
        if self.enabled == Some(false) {
            return CollectionPhase::Off;
        }
        match self.state {
            CollectionState::CaughtUp => CollectionPhase::CaughtUp {
                at: self.completed_at,
            },
            CollectionState::Scanning => CollectionPhase::Reading {
                files: self.pending_files,
            },
            // No pass has got as far as reading. Which of the rest it is
            // takes knowing that collection is on, and then how far the
            // walk has got.
            CollectionState::Unknown => match (self.enabled, self.discovery_generation) {
                (None, _) => CollectionPhase::Unknown,
                (Some(_), None) => CollectionPhase::Starting,
                (Some(_), Some(_)) if !self.discovery_ended => CollectionPhase::Discovering,
                (Some(_), Some(_)) if !self.discovery_complete => {
                    CollectionPhase::DiscoveryIncomplete
                }
                // Not something the collector writes (a complete walk is
                // stored as scanning in the same step); read as what a
                // complete walk is followed by.
                (Some(_), Some(_)) => CollectionPhase::Reading {
                    files: self.pending_files,
                },
            },
        }
    }
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
    /// Each agent's latest context use, by agent id, where a provider
    /// record states it: the sessions in the query's scope, whatever its
    /// range, and nothing for a session no record speaks for. Absent on
    /// older daemons and while collection is off.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub context: BTreeMap<String, Context>,
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
    /// Whether any row has an estimated cost: the column is shown only
    /// then, since a table with no price matched has nothing to say.
    pub fn priced(&self) -> bool {
        self.rows.iter().any(|row| row.estimated_cost.is_some())
    }

    /// Each row's tokens by its key. A row no sample gave a count for, and
    /// unattributed usage, are left out rather than shown as zero.
    pub fn tokens(&self) -> BTreeMap<String, Tokens> {
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
    /// every sample reported both, never over a hundred, and absent for
    /// no input.
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
            Row::cache_read_share(1, &counters(Some(400), Some(300))),
            Some(75)
        );
        assert_eq!(
            Row::cache_read_share(1, &counters(Some(3), Some(1))),
            Some(33)
        );
        // Two samples that each reported one counter: both counts are one,
        // yet no sample reported both, and no share is made of them.
        assert_eq!(
            Row::cache_read_share(2, &counters(Some(100), Some(50))),
            None
        );
        // A read known on a sample whose total input is not, beside one
        // that reported both: no share either.
        let mut uneven = counters(Some(400), Some(300));
        uneven.cache_read_input_tokens.known_samples = 2;
        uneven.cache_read_input_tokens.sum = Some(500);
        assert_eq!(Row::cache_read_share(2, &uneven), None);
        // Both known on every sample: the share.
        let mut paired = counters(Some(400), Some(300));
        paired.input_tokens.known_samples = 2;
        paired.cache_read_input_tokens.known_samples = 2;
        assert_eq!(Row::cache_read_share(2, &paired), Some(75));
        assert_eq!(
            Row::cache_read_share(1, &counters(Some(10), Some(20))),
            Some(100)
        );
        assert_eq!(Row::cache_read_share(1, &counters(Some(0), Some(0))), None);
        // Counts whose hundredfold overflows a u64 keep their share.
        assert_eq!(
            Row::cache_read_share(1, &counters(Some(10_u64.pow(19)), Some(10_u64.pow(18)))),
            Some(10)
        );
        assert_eq!(
            Row::cache_read_share(1, &counters(Some(u64::MAX), Some(u64::MAX / 2))),
            Some(49)
        );
        assert_eq!(
            Row::cache_read_share(1, &counters(Some(u64::MAX), Some(u64::MAX))),
            Some(100)
        );
        assert_eq!(Row::cache_read_share(1, &counters(None, Some(5))), None);
        assert_eq!(Row::cache_read_share(1, &counters(Some(5), None)), None);
        let older: Row = serde_json::from_value(serde_json::json!({
            "key": null, "samples": 1,
            "counters": serde_json::to_value(counters(Some(5), None)).unwrap()
        }))
        .unwrap();
        assert_eq!(older.cost_usd_micros, CounterReport::default());
        assert_eq!(older.cache_read_share_percent, None);
    }

    fn collection(
        enabled: Option<bool>,
        state: CollectionState,
        generation: Option<u64>,
        ended: bool,
        complete: bool,
    ) -> Collection {
        Collection {
            enabled,
            state,
            discovery_generation: generation,
            discovery_ended: ended,
            discovery_complete: complete,
            ..Collection::default()
        }
    }

    /// Where a collection stands is one reading of its fields. Each row
    /// is a collection as the daemon answers it, and the phase a person
    /// is told. The third and fourth rows were one row: a walk under way
    /// and a walk that stopped early had the same fields.
    #[test]
    fn a_collections_phase_is_one_reading_of_its_fields() {
        use CollectionPhase as P;
        use CollectionState as S;
        let at = "2026-01-02T03:04:05Z".parse::<DateTime<Utc>>().unwrap();
        let reading = Collection {
            pending_files: Some(7),
            ..collection(Some(true), S::Scanning, Some(4), true, true)
        };
        let caught_up = Collection {
            pending_files: Some(0),
            completed_at: Some(at),
            ..collection(Some(true), S::CaughtUp, Some(4), true, true)
        };
        let table = [
            // Switched off: what the daemon answers is the default with
            // `enabled` false.
            (
                collection(Some(false), S::Unknown, None, false, false),
                P::Off,
            ),
            // On, and nothing collected yet; also what a change of roots
            // answers until its first pass.
            (
                collection(Some(true), S::Unknown, None, false, false),
                P::Starting,
            ),
            // A pass walking its roots.
            (
                collection(Some(true), S::Unknown, Some(4), false, false),
                P::Discovering,
            ),
            // Its walk ended early.
            (
                collection(Some(true), S::Unknown, Some(4), true, false),
                P::DiscoveryIncomplete,
            ),
            // The walk complete, files to read.
            (reading.clone(), P::Reading { files: Some(7) }),
            // A collection stored before `discovery_ended`, reading.
            (
                collection(Some(true), S::Scanning, Some(4), false, true),
                P::Reading { files: None },
            ),
            (caught_up.clone(), P::CaughtUp { at: Some(at) }),
            (
                collection(Some(true), S::CaughtUp, Some(4), true, true),
                P::CaughtUp { at: None },
            ),
            // A daemon from before `enabled`: its state is what it says,
            // and without one nothing is told. It is not called off, and
            // not called discovering on a generation alone.
            (collection(None, S::Unknown, None, false, false), P::Unknown),
            (
                collection(None, S::Unknown, Some(4), false, false),
                P::Unknown,
            ),
            (
                collection(None, S::Unknown, Some(4), true, false),
                P::Unknown,
            ),
            (
                collection(None, S::Scanning, Some(4), false, true),
                P::Reading { files: None },
            ),
            (
                collection(None, S::CaughtUp, Some(4), false, true),
                P::CaughtUp { at: None },
            ),
            // Not something the collector writes: a complete walk whose
            // state has not moved on.
            (
                collection(Some(true), S::Unknown, Some(4), true, true),
                P::Reading { files: None },
            ),
        ];
        for (collection, phase) in &table {
            assert_eq!(collection.phase(), *phase, "{collection:?}");
        }

        // Over every combination: off is the configuration's word and
        // nothing else's, and unknown is said only of a report that does
        // not say whether collection is on.
        let mut words = std::collections::BTreeSet::new();
        for enabled in [None, Some(false), Some(true)] {
            for state in [S::Unknown, S::Scanning, S::CaughtUp] {
                for generation in [None, Some(4)] {
                    for ended in [false, true] {
                        for complete in [false, true] {
                            let phase =
                                collection(enabled, state, generation, ended, complete).phase();
                            assert_eq!(phase == P::Off, enabled == Some(false));
                            assert_eq!(
                                phase == P::Unknown,
                                enabled.is_none() && state == S::Unknown
                            );
                            if enabled == Some(true) {
                                assert_eq!(
                                    matches!(phase, P::CaughtUp { .. }),
                                    state == S::CaughtUp,
                                    "caught up is the stored state's alone"
                                );
                            }
                            words.insert(phase.word());
                        }
                    }
                }
            }
        }
        assert_eq!(
            words.into_iter().collect::<Vec<_>>(),
            [
                "caught up",
                "discovering",
                "discovery incomplete",
                "off",
                "reading",
                "starting",
                "unknown"
            ],
            "every phase is reached, each with a word of its own"
        );
        // Only the phase whose word does not say what it means for the
        // totals explains itself.
        assert!(P::DiscoveryIncomplete.note().is_some());
        for phase in [
            P::Off,
            P::Starting,
            P::Discovering,
            P::Reading { files: None },
            P::CaughtUp { at: None },
            P::Unknown,
        ] {
            assert_eq!(phase.note(), None);
        }
    }

    /// The one new stored fact is additive: a collection without it is
    /// read as before, and one whose walk has not ended is written as
    /// before, so the stored document and the wire keep their shape.
    #[test]
    fn a_walks_end_is_an_additive_field() {
        let before = serde_json::to_value(Collection::default()).unwrap();
        assert!(before.get("discovery_ended").is_none());
        let read: Collection = serde_json::from_value(before.clone()).unwrap();
        assert!(!read.discovery_ended);
        assert_eq!(read, Collection::default());

        let ended = Collection {
            discovery_ended: true,
            ..Collection::default()
        };
        let wire = serde_json::to_value(&ended).unwrap();
        assert_eq!(wire["discovery_ended"], serde_json::Value::Bool(true));
        assert_eq!(serde_json::from_value::<Collection>(wire).unwrap(), ended);
        // The stored state keeps its three words.
        assert_eq!(before["state"], "unknown");
    }
}
