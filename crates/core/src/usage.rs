//! Token accounting without provider I/O or an implicit wall clock.
//!
//! Input/output totals include their reported cache/reasoning components.
//! Unknown counters stay optional; a known zero is not missing data.

use chrono::{DateTime, Duration, Timelike, Utc};
use serde::{Deserialize, Serialize};

pub mod emission;
pub mod report;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Counters {
    pub input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
}

impl Counters {
    /// A cumulative decrease begins a new epoch, never a negative contribution.
    /// A field lacking either baseline is unknown even if other fields advanced.
    pub fn delta(&self, previous: &Self) -> Option<Self> {
        let current = self.values();
        let previous = previous.values();
        if current
            .iter()
            .zip(previous)
            .any(|(now, old)| matches!((now, old), (Some(now), Some(old)) if *now < old))
        {
            return None;
        }
        Some(Self::from_values(std::array::from_fn(|i| {
            current[i]?.checked_sub(previous[i]?)
        })))
    }

    pub fn values(&self) -> [Option<u64>; 5] {
        [
            self.input_tokens,
            self.cache_read_input_tokens,
            self.cache_write_input_tokens,
            self.output_tokens,
            self.reasoning_output_tokens,
        ]
    }

    pub fn from_values(values: [Option<u64>; 5]) -> Self {
        Self {
            input_tokens: values[0],
            cache_read_input_tokens: values[1],
            cache_write_input_tokens: values[2],
            output_tokens: values[3],
            reasoning_output_tokens: values[4],
        }
    }
}

/// Durable cumulative state belongs to a runtime/session, not its current
/// agent display name or log path. Keep it even after aggregate retention.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Baseline {
    pub at: DateTime<Utc>,
    pub counters: Counters,
    pub epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gap {
    /// None means the earlier history is unknown, not that it began now.
    pub since: Option<DateTime<Utc>>,
    pub until: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    pub baseline: Baseline,
    pub contribution: Option<Counters>,
    pub gap: Option<Gap>,
}

/// Propose the state and contribution together. The caller deduplicates source
/// identities and commits this result atomically with its file cursor. An
/// out-of-order or conflicting timestamp needs explicit source reconciliation;
/// it cannot silently replace the current baseline or subtract usage. An exact
/// replay returns None without proposing a state change.
pub fn observe_cumulative(
    previous: Option<&Baseline>,
    at: DateTime<Utc>,
    counters: Counters,
    proves_zero_baseline: bool,
) -> Result<Option<Observation>, &'static str> {
    let mut baseline = Baseline {
        at,
        counters,
        epoch: 0,
    };
    let (contribution, gap) = if let Some(previous) = previous {
        if at < previous.at {
            return Err("cumulative snapshot precedes its durable baseline");
        }
        if at == previous.at {
            return if baseline.counters == previous.counters {
                Ok(None)
            } else {
                Err("cumulative snapshots disagree at the same timestamp")
            };
        }
        baseline.epoch = previous.epoch;
        match baseline.counters.delta(&previous.counters) {
            Some(delta) => (Some(delta), None),
            None => {
                baseline.epoch = baseline
                    .epoch
                    .checked_add(1)
                    .ok_or("counter epoch overflows")?;
                (
                    None,
                    Some(Gap {
                        since: Some(previous.at),
                        until: at,
                    }),
                )
            }
        }
    } else if proves_zero_baseline {
        (Some(baseline.counters.clone()), None)
    } else {
        (
            None,
            Some(Gap {
                since: None,
                until: at,
            }),
        )
    };
    Ok(Some(Observation {
        baseline,
        contribution,
        gap,
    }))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Total {
    sum: u64,
    known_samples: u64,
}

/// Where a counted figure comes from: the provider's own count for one
/// response, or the difference between two of its running totals, which
/// a reset or a gap between them makes an approximation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    Exact,
    Estimated,
}

/// A bucket retains known/sample counts with the sums, so moving historical
/// attribution never turns unknown fields into zero or loses coverage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "StoredAggregate")]
pub struct Aggregate {
    samples: u64,
    totals: [Total; 5],
    /// How many of `samples` are a provider's own per-response figure.
    /// `None` once a bucket written before this was counted is in it: a
    /// count that cannot say is never read as all estimated.
    exact: Option<u64>,
    /// The provider-reported estimated cost of the samples that carried one, in
    /// micro-dollars (USD × 10⁶). Absent from buckets written before cost
    /// was collected, which read as a bucket no sample priced.
    #[serde(default, skip_serializing_if = "Total::is_empty")]
    cost: Total,
}

impl Default for Aggregate {
    fn default() -> Self {
        Self {
            samples: 0,
            totals: [Total::default(); 5],
            exact: Some(0),
            cost: Total::default(),
        }
    }
}

impl Total {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredAggregate {
    samples: u64,
    totals: [Total; 5],
    #[serde(default)]
    exact: Option<u64>,
    #[serde(default)]
    cost: Total,
}

impl TryFrom<StoredAggregate> for Aggregate {
    type Error = &'static str;

    fn try_from(stored: StoredAggregate) -> Result<Self, Self::Error> {
        let aggregate = Self {
            samples: stored.samples,
            totals: stored.totals,
            exact: stored.exact,
            cost: stored.cost,
        };
        aggregate.validate()?;
        Ok(aggregate)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    Complete,
    Partial,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterReport {
    pub sum: Option<u64>,
    pub known_samples: u64,
    pub coverage: Coverage,
}

impl Default for CounterReport {
    /// What a counter no sample reported looks like, and what an older
    /// daemon's report reads as for a counter it did not have.
    fn default() -> Self {
        Self {
            sum: None,
            known_samples: 0,
            coverage: Coverage::Unknown,
        }
    }
}

impl Aggregate {
    fn validate(&self) -> Result<(), &'static str> {
        if self.totals.iter().chain([&self.cost]).any(|total| {
            total.known_samples > self.samples || (total.known_samples == 0 && total.sum != 0)
        }) {
            return Err("usage bucket coverage is inconsistent");
        }
        if self.exact.is_some_and(|exact| exact > self.samples) {
            return Err("usage bucket coverage is inconsistent");
        }
        Ok(())
    }

    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// `Exact` when every counted sample is a provider's own per-response
    /// figure, `Estimated` when at least one is not; `None` for an empty
    /// bucket or one that cannot say.
    pub fn source(&self) -> Option<UsageSource> {
        match self.exact {
            Some(exact) if self.samples > 0 && exact == self.samples => Some(UsageSource::Exact),
            Some(_) if self.samples > 0 => Some(UsageSource::Estimated),
            _ => None,
        }
    }

    /// Merge disjoint hourly buckets while preserving unknown/sample counts.
    /// Overflow refuses the entire proposal; neither input is changed.
    pub fn merge(&self, other: &Self) -> Result<Self, &'static str> {
        let mut next = self.clone();
        next.samples = next
            .samples
            .checked_add(other.samples)
            .ok_or("usage bucket arithmetic is out of range")?;
        for (total, added) in next
            .totals
            .iter_mut()
            .chain([&mut next.cost])
            .zip(other.totals.iter().chain([&other.cost]))
        {
            total.sum = total
                .sum
                .checked_add(added.sum)
                .ok_or("usage bucket arithmetic is out of range")?;
            total.known_samples = total
                .known_samples
                .checked_add(added.known_samples)
                .ok_or("usage bucket arithmetic is out of range")?;
        }
        next.exact = match (self.exact, other.exact) {
            (Some(mine), Some(theirs)) => Some(
                mine.checked_add(theirs)
                    .ok_or("usage bucket arithmetic is out of range")?,
            ),
            _ => None,
        };
        next.validate()?;
        Ok(next)
    }

    /// Return a proposed replacement; an overflow/invalid removal cannot
    /// partially modify an existing bucket. Persistence commits it separately.
    /// `exact` says whether the sample is a provider's own per-response
    /// figure; a sample that cannot say leaves the bucket unable to.
    pub fn add(&self, counters: &Counters, exact: Option<bool>) -> Result<Self, &'static str> {
        self.change(counters, exact, None, false)
    }

    pub fn remove(&self, counters: &Counters, exact: Option<bool>) -> Result<Self, &'static str> {
        self.change(counters, exact, None, true)
    }

    /// [`Self::add`] for a sample that also carried its price, in
    /// micro-dollars; `None` is a sample that did not say what it cost.
    pub fn add_priced(
        &self,
        counters: &Counters,
        exact: Option<bool>,
        cost_usd_micros: Option<u64>,
    ) -> Result<Self, &'static str> {
        self.change(counters, exact, cost_usd_micros, false)
    }

    pub fn remove_priced(
        &self,
        counters: &Counters,
        exact: Option<bool>,
        cost_usd_micros: Option<u64>,
    ) -> Result<Self, &'static str> {
        self.change(counters, exact, cost_usd_micros, true)
    }

    /// A price learnt after the sample was counted (the provider's event
    /// arrived after its transcript record): the bucket's cost takes it,
    /// its tokens unchanged. The sample must already be in the bucket.
    pub fn price(&self, cost_usd_micros: u64) -> Result<Self, &'static str> {
        let mut next = self.clone();
        next.cost.sum = next
            .cost
            .sum
            .checked_add(cost_usd_micros)
            .ok_or("usage bucket arithmetic is out of range")?;
        next.cost.known_samples = next
            .cost
            .known_samples
            .checked_add(1)
            .ok_or("usage bucket arithmetic is out of range")?;
        next.validate()?;
        Ok(next)
    }

    fn change(
        &self,
        counters: &Counters,
        exact: Option<bool>,
        cost_usd_micros: Option<u64>,
        remove: bool,
    ) -> Result<Self, &'static str> {
        let change = |a: u64, b: u64| {
            if remove {
                a.checked_sub(b)
            } else {
                a.checked_add(b)
            }
            .ok_or("usage bucket arithmetic is out of range")
        };
        let mut next = self.clone();
        next.samples = change(next.samples, 1)?;
        next.exact = match (self.exact, exact) {
            (Some(count), Some(true)) => Some(change(count, 1)?),
            (Some(count), Some(false)) => Some(count),
            _ => None,
        };
        for (total, value) in next
            .totals
            .iter_mut()
            .chain([&mut next.cost])
            .zip(counters.values().into_iter().chain([cost_usd_micros]))
        {
            if let Some(value) = value {
                total.sum = change(total.sum, value)?;
                total.known_samples = change(total.known_samples, 1)?;
            }
        }
        next.validate()?;
        Ok(next)
    }

    /// The bucket's price with its coverage, on the same terms as
    /// [`Self::counters`]: a sum only over the samples that carried one.
    pub fn cost(&self, source_complete: bool) -> CounterReport {
        let total = self.cost;
        CounterReport {
            sum: (total.known_samples > 0).then_some(total.sum),
            known_samples: total.known_samples,
            coverage: if total.known_samples == 0 {
                Coverage::Unknown
            } else if source_complete && total.known_samples == self.samples {
                Coverage::Complete
            } else {
                Coverage::Partial
            },
        }
    }

    /// `source_complete` means the caller proved completed collection and no
    /// relevant gaps for this row's scope/range, not just all known samples.
    pub fn counters(&self, source_complete: bool) -> [CounterReport; 5] {
        self.totals.map(|total| CounterReport {
            sum: (total.known_samples > 0).then_some(total.sum),
            known_samples: total.known_samples,
            coverage: if total.known_samples == 0 {
                Coverage::Unknown
            } else if source_complete && total.known_samples == self.samples {
                Coverage::Complete
            } else {
                Coverage::Partial
            },
        })
    }
}

/// Effective hourly bounds and the reasons they differ from a requested range.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub as_of: DateTime<Utc>,
    pub effective_since: DateTime<Utc>,
    pub effective_until: DateTime<Utc>,
    pub retained_since: DateTime<Utc>,
    pub history_truncated: bool,
    pub future_until_clamped: bool,
    pub includes_current_hour: bool,
}

pub fn hour(at: DateTime<Utc>) -> DateTime<Utc> {
    at.with_minute(0)
        .unwrap()
        .with_second(0)
        .unwrap()
        .with_nanosecond(0)
        .unwrap()
}

/// The usage command intentionally accepts a narrower duration grammar than
/// retention configuration: one positive integer and exactly one of s/m/h/d.
pub fn since(text: &str, as_of: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    if let Ok(at) = DateTime::parse_from_rfc3339(text) {
        return Ok(at.with_timezone(&Utc));
    }
    if !text.is_ascii() {
        return Err("usage duration must use ASCII digits and s/m/h/d".into());
    }
    let (digits, unit) = text.split_at(text.len().saturating_sub(1));
    let factor = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => return Err("expected an RFC3339 timestamp or positive integer with s/m/h/d".into()),
    };
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return Err("usage duration must be a positive integer with s/m/h/d".into());
    }
    let seconds = digits
        .parse::<i64>()
        .ok()
        .and_then(|n| n.checked_mul(factor))
        .filter(|n| *n > 0)
        .ok_or("usage duration is zero or overflows")?;
    let duration = Duration::try_seconds(seconds).ok_or("usage duration overflows")?;
    as_of
        .checked_sub_signed(duration)
        .ok_or_else(|| "usage duration is out of range".into())
}

impl Range {
    /// Validate before rounding, then clamp to the retained hourly boundary.
    pub fn new(
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
        as_of: DateTime<Utc>,
        retained_since: DateTime<Utc>,
    ) -> Result<Self, String> {
        let since = match since {
            Some(since) => since,
            None => as_of
                .checked_sub_signed(Duration::hours(24))
                .ok_or("default usage range is out of range")?,
        };
        let until = until.unwrap_or(as_of);
        if since >= until || since > as_of {
            return Err("usage range must be nonempty, ordered and begin no later than now".into());
        }
        if retained_since != hour(retained_since) || retained_since > hour(as_of) {
            return Err(
                "retention boundary must be a UTC hour no later than the current hour".into(),
            );
        }
        let future_until_clamped = until > as_of;
        let until = until.min(as_of);
        if since >= until {
            return Err("usage range contains no elapsed time".into());
        }
        let start = hour(since);
        let end = if hour(until) == until {
            until
        } else {
            hour(until)
                .checked_add_signed(Duration::hours(1))
                .ok_or("usage upper bound overflows")?
        };
        let effective_since = start.max(retained_since);
        let effective_until = end.max(retained_since);
        Ok(Self {
            as_of,
            effective_since,
            effective_until,
            retained_since,
            history_truncated: since < retained_since,
            future_until_clamped,
            includes_current_hour: effective_since <= hour(as_of) && effective_until > hour(as_of),
        })
    }
}

/// Which registration of one provider session a sample produced at `at`
/// belongs to. A session resumed after its agent ended registers again
/// under a new agent with the same session ID, so one session can have
/// several registrations, each given as `(key, created_at)`. The sample is
/// the one current at `at`: the latest registered at or before it, or the
/// first when the sample predates them all (a transcript begins before
/// its session registers). Two registrations created at the same moment
/// cannot be told apart, and neither is chosen.
pub fn registration_at<K: Clone>(
    registrations: &[(K, DateTime<Utc>)],
    at: DateTime<Utc>,
) -> Option<K> {
    let current = registrations
        .iter()
        .filter(|(_, created)| *created <= at)
        .map(|(_, created)| *created)
        .max()
        .or_else(|| registrations.iter().map(|(_, created)| *created).min())?;
    let mut chosen = registrations
        .iter()
        .filter(|(_, created)| *created == current);
    let (key, _) = chosen.next()?;
    chosen.next().is_none().then(|| key.clone())
}

/// The same choice as [`registration_at`], as the span of sample times
/// each registration owns: `[from, until)`, open-ended where `None`. The
/// first registration also owns everything before it; registrations
/// created at the same moment own nothing.
pub type Window<K> = (K, Option<DateTime<Utc>>, Option<DateTime<Utc>>);

pub fn registration_windows<K: Clone>(registrations: &[(K, DateTime<Utc>)]) -> Vec<Window<K>> {
    let mut times: Vec<_> = registrations.iter().map(|(_, created)| *created).collect();
    times.sort();
    times.dedup();
    times
        .iter()
        .enumerate()
        .filter_map(|(i, time)| {
            let mut owners = registrations.iter().filter(|(_, created)| created == time);
            let (key, _) = owners.next()?;
            owners.next().is_none().then(|| {
                let from = (i > 0).then_some(*time);
                (key.clone(), from, times.get(i + 1).copied())
            })
        })
        .collect()
}

/// A large count in three figures: `9,999`, `12.3K`, `1.23M`. Exact
/// below ten thousand. One format for the desktop and the CLI.
pub fn compact(value: u64) -> String {
    if value < 10_000 {
        return thousands(value);
    }
    let mut scaled = value as f64;
    for unit in ["K", "M", "B", "T"] {
        scaled /= 1000.0;
        let digits = if scaled < 10.0 {
            2
        } else if scaled < 100.0 {
            1
        } else {
            0
        };
        let rounded = format!("{scaled:.digits$}");
        if rounded.parse::<f64>().is_ok_and(|r| r < 1000.0) || unit == "T" {
            return format!("{rounded}{unit}");
        }
    }
    thousands(value)
}

/// A price in micro-dollars as a person reads it: `$0.0123` below a
/// dollar, `$1.23` to a thousand, then `$12,345.68`. Four places under a
/// dollar because a single request costs cents or less.
pub fn dollars(usd_micros: u64) -> String {
    if usd_micros < 1_000_000 {
        return format!("${:.4}", usd_micros as f64 / 1e6);
    }
    let cents = usd_micros.div_ceil(10_000);
    let (whole, part) = (cents / 100, cents % 100);
    format!("${}.{part:02}", thousands(whole))
}

/// `12345678` as `12,345,678`.
pub fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    /// A token count shortens to three figures without losing its scale.
    #[test]
    fn token_counts_shorten_to_three_figures() {
        assert_eq!(compact(950), "950");
        assert_eq!(compact(9_999), "9,999");
        assert_eq!(compact(12_345), "12.3K");
        assert_eq!(compact(123_456), "123K");
        assert_eq!(compact(999_950), "1.00M");
        assert_eq!(compact(1_234_567), "1.23M");
        assert_eq!(compact(846_238_624), "846M");
        assert_eq!(compact(45_600_000_000), "45.6B");
    }

    use super::*;

    #[test]
    fn a_sample_belongs_to_the_registration_current_when_it_was_made() {
        let first = at("2026-09-21T10:00:00Z");
        let resumed = at("2026-09-22T09:00:00Z");
        let both = [("ended", first), ("resumed", resumed)];
        assert_eq!(registration_at(&[("only", first)], resumed), Some("only"));
        assert_eq!(
            registration_at(&both, at("2026-09-21T12:00:00Z")),
            Some("ended")
        );
        assert_eq!(registration_at(&both, resumed), Some("resumed"));
        assert_eq!(
            registration_at(&both, at("2026-09-23T01:00:00Z")),
            Some("resumed")
        );
        // Before any registration: the session's first one.
        assert_eq!(
            registration_at(&both, at("2026-09-20T00:00:00Z")),
            Some("ended")
        );
        // Order does not matter.
        let reversed = [("resumed", resumed), ("ended", first)];
        assert_eq!(
            registration_at(&reversed, at("2026-09-21T12:00:00Z")),
            Some("ended")
        );
        // Indistinguishable registrations, and none at all, choose nobody.
        let twins = [("a", first), ("b", first)];
        assert_eq!(registration_at(&twins, at("2026-09-21T12:00:00Z")), None);
        assert_eq!(registration_at::<&str>(&[], resumed), None);
    }

    /// Windows agree with the per-sample choice at every boundary.
    #[test]
    fn registration_windows_partition_time_as_registration_at_does() {
        let t = |h: u32| at(&format!("2026-09-22T{h:02}:00:00Z"));
        let registrations = [("c", t(9)), ("a", t(1)), ("x", t(5)), ("y", t(5))];
        let windows = registration_windows(&registrations);
        assert_eq!(
            windows,
            vec![("a", None, Some(t(5))), ("c", Some(t(9)), None)]
        );
        for hour in 0..12 {
            let owner = windows
                .iter()
                .find(|(_, from, until)| {
                    from.is_none_or(|f| f <= t(hour)) && until.is_none_or(|u| t(hour) < u)
                })
                .map(|(key, ..)| *key);
            assert_eq!(
                owner,
                registration_at(&registrations, t(hour)),
                "hour {hour}"
            );
        }
        assert!(registration_windows::<&str>(&[]).is_empty());
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn deltas_keep_unknowns_and_zero_and_reject_counter_resets() {
        let old = Counters::from_values([Some(100), Some(50), None, Some(20), Some(10)]);
        let new = Counters::from_values([Some(100), Some(50), Some(5), Some(23), None]);
        assert_eq!(
            new.delta(&old).unwrap().values(),
            [Some(0), Some(0), None, Some(3), None]
        );
        let reset = Counters {
            input_tokens: Some(99),
            ..new
        };
        assert!(reset.delta(&old).is_none());
    }

    #[test]
    fn cumulative_restart_reset_and_replay_keep_unknown_history_explicit() {
        let start = at("2026-09-16T12:00:00Z");
        let counters = |input| Counters {
            input_tokens: Some(input),
            ..Counters::default()
        };
        let Some(Observation {
            baseline,
            contribution,
            gap,
        }) = observe_cumulative(None, start, counters(100), false).unwrap()
        else {
            panic!("first snapshot")
        };
        assert_eq!(contribution, None);
        assert_eq!(
            gap,
            Some(Gap {
                since: None,
                until: start
            })
        );
        let saved = serde_json::to_string(&baseline).unwrap();
        let resumed: Baseline = serde_json::from_str(&saved).unwrap();
        assert_eq!(
            observe_cumulative(Some(&resumed), start, counters(100), true).unwrap(),
            None
        );
        assert!(observe_cumulative(Some(&resumed), start, counters(101), false).is_err());
        assert!(
            observe_cumulative(
                Some(&resumed),
                start - Duration::seconds(1),
                counters(99),
                false
            )
            .is_err()
        );
        let reset_at = start + Duration::hours(1);
        let Some(Observation {
            baseline: reset,
            contribution,
            gap,
        }) = observe_cumulative(Some(&resumed), reset_at, counters(2), false).unwrap()
        else {
            panic!("reset")
        };
        assert_eq!(reset.epoch, 1);
        assert_eq!(contribution, None);
        assert_eq!(
            gap,
            Some(Gap {
                since: Some(start),
                until: reset_at
            })
        );
        let Some(Observation {
            baseline: next,
            contribution,
            gap,
        }) = observe_cumulative(
            Some(&reset),
            reset_at + Duration::minutes(1),
            counters(7),
            false,
        )
        .unwrap()
        else {
            panic!("next")
        };
        assert_eq!(next.epoch, 1);
        assert_eq!(contribution, Some(counters(5)));
        assert_eq!(gap, None);
        let Some(Observation {
            contribution, gap, ..
        }) = observe_cumulative(None, start, counters(100), true).unwrap()
        else {
            panic!("proved start")
        };
        assert_eq!(contribution, Some(counters(100)));
        assert_eq!(gap, None);
    }

    #[test]
    fn restored_aggregates_cannot_invent_counter_coverage() {
        let bucket = Aggregate::default()
            .add(
                &Counters {
                    input_tokens: Some(8),
                    output_tokens: Some(0),
                    ..Counters::default()
                },
                Some(false),
            )
            .unwrap();
        let stored = serde_json::to_value(&bucket).unwrap();
        assert_eq!(
            serde_json::from_value::<Aggregate>(stored.clone()).unwrap(),
            bucket
        );
        let mut corrupt = stored.clone();
        corrupt["totals"][0]["known_samples"] = serde_json::json!(2);
        assert!(serde_json::from_value::<Aggregate>(corrupt).is_err());
        let mut corrupt = stored;
        corrupt["totals"][0]["known_samples"] = serde_json::json!(0);
        assert!(serde_json::from_value::<Aggregate>(corrupt).is_err());
        let unknown = Aggregate::default()
            .add(&Counters::default(), Some(false))
            .unwrap();
        let restored: Aggregate =
            serde_json::from_value(serde_json::to_value(&unknown).unwrap()).unwrap();
        assert!(
            restored
                .counters(true)
                .iter()
                .all(|counter| counter.sum.is_none() && counter.coverage == Coverage::Unknown)
        );
    }

    #[test]
    fn aggregate_moves_unknowns_and_known_zero_with_their_contributions() {
        let zero = Counters::from_values([Some(0), None, None, Some(0), None]);
        let known = Counters::from_values([Some(10), Some(2), None, Some(3), Some(1)]);
        let original = Aggregate::default()
            .add(&zero, Some(false))
            .unwrap()
            .add(&known, Some(false))
            .unwrap();
        assert_eq!(original.samples(), 2);
        let complete = original.counters(true);
        assert_eq!(complete[0].sum, Some(10));
        assert_eq!(complete[0].coverage, Coverage::Complete);
        assert_eq!(complete[1].coverage, Coverage::Partial);
        assert_eq!(complete[2].coverage, Coverage::Unknown);
        assert_eq!(complete[2].sum, None);
        assert_eq!(original.counters(false)[0].coverage, Coverage::Partial);
        let source = original.remove(&known, Some(false)).unwrap();
        let destination = Aggregate::default().add(&known, Some(false)).unwrap();
        assert_eq!(source.counters(true)[0].sum, Some(0));
        assert_eq!(source.counters(true)[1].sum, None);
        assert_eq!(source.samples() + destination.samples(), original.samples());
        assert_eq!(source.add(&known, Some(false)).unwrap(), original);
        assert_eq!(
            source.remove(&zero, Some(false)).unwrap(),
            Aggregate::default()
        );
    }

    #[test]
    fn aggregate_says_whether_its_figures_are_exact() {
        let known = Counters::from_values([Some(10), Some(2), None, Some(3), Some(1)]);
        assert_eq!(Aggregate::default().source(), None);
        let exact = Aggregate::default().add(&known, Some(true)).unwrap();
        assert_eq!(exact.source(), Some(UsageSource::Exact));
        let mixed = exact.add(&known, Some(false)).unwrap();
        assert_eq!(mixed.source(), Some(UsageSource::Estimated));
        assert_eq!(
            mixed.remove(&known, Some(false)).unwrap().source(),
            Some(UsageSource::Exact)
        );
        // A bucket written before the count existed cannot say, and nothing
        // merged into it or added to it makes it say.
        let mut stored = serde_json::to_value(&exact).unwrap();
        stored.as_object_mut().unwrap().remove("exact");
        let older: Aggregate = serde_json::from_value(stored).unwrap();
        assert_eq!(older.source(), None);
        assert_eq!(older.add(&known, Some(true)).unwrap().source(), None);
        assert_eq!(exact.merge(&older).unwrap().source(), None);
        assert_eq!(
            exact.merge(&mixed).unwrap().source(),
            Some(UsageSource::Estimated)
        );
        assert_eq!(exact.add(&known, None).unwrap().source(), None);
        let mut corrupt = serde_json::to_value(&exact).unwrap();
        corrupt["exact"] = serde_json::json!(5);
        assert!(serde_json::from_value::<Aggregate>(corrupt).is_err());
    }

    /// Cost rides with a sample as its own total: a priced sample counts
    /// its price, an unpriced one leaves the price unknown, a price learnt
    /// late joins without recounting tokens, a removal takes it back, and
    /// buckets written before cost existed read as unpriced.
    #[test]
    fn cost_is_a_counter_of_its_own_with_coverage_and_late_pricing() {
        let tokens = Counters::from_values([Some(10), None, None, Some(3), None]);
        let priced = Aggregate::default()
            .add_priced(&tokens, Some(true), Some(1_500))
            .unwrap();
        assert_eq!(priced.cost(true).sum, Some(1_500));
        assert_eq!(priced.cost(true).coverage, Coverage::Complete);
        let mixed = priced.add(&tokens, Some(true)).unwrap();
        assert_eq!(mixed.cost(true).sum, Some(1_500));
        assert_eq!(mixed.cost(true).coverage, Coverage::Partial);
        let late = mixed.price(500).unwrap();
        assert_eq!(late.cost(true).sum, Some(2_000));
        assert_eq!(late.cost(true).coverage, Coverage::Complete);
        assert_eq!(late.counters(true)[0].sum, Some(20), "tokens unchanged");
        assert!(Aggregate::default().price(1).is_err(), "nothing to price");
        let back = late.remove_priced(&tokens, Some(true), Some(500)).unwrap();
        assert_eq!(back.cost(true), priced.cost(true));
        assert_eq!(back.samples(), 1);
        let legacy: Aggregate = serde_json::from_value(serde_json::json!({
            "samples": 1,
            "totals": [{"sum": 10, "known_samples": 1}, {"sum": 0, "known_samples": 0},
                {"sum": 0, "known_samples": 0}, {"sum": 3, "known_samples": 1}, {"sum": 0, "known_samples": 0}]
        }))
        .unwrap();
        assert_eq!(legacy.cost(true), CounterReport::default());
        assert_eq!(legacy.merge(&priced).unwrap().cost(true).sum, Some(1_500));
        let unpriced = serde_json::to_value(&legacy).unwrap();
        assert!(
            unpriced.get("cost").is_none(),
            "an unpriced bucket keeps its old shape"
        );
        assert_eq!(dollars(0), "$0.0000");
        assert_eq!(dollars(12_345), "$0.0123");
        assert_eq!(dollars(999_999), "$1.0000");
        assert_eq!(dollars(1_234_567), "$1.24");
        assert_eq!(dollars(12_345_678_900), "$12,345.68");
    }

    #[test]
    fn rejected_bucket_arithmetic_preserves_original_sums_and_coverage() {
        let huge = Counters {
            input_tokens: Some(u64::MAX),
            ..Counters::default()
        };
        let original = Aggregate::default().add(&huge, Some(false)).unwrap();
        let saved = original.clone();
        assert!(original.add(&huge, Some(false)).is_err());
        assert!(original.remove(&Counters::default(), Some(false)).is_err());
        assert_eq!(original, saved);
        assert_eq!(
            original.remove(&huge, Some(false)).unwrap(),
            Aggregate::default()
        );
        assert!(Aggregate::default().remove(&huge, Some(false)).is_err());
    }

    #[test]
    fn duration_grammar_is_strict_and_never_wraps() {
        let now = at("2026-09-16T12:30:00Z");
        assert_eq!(since("24h", now).unwrap(), now - Duration::hours(24));
        assert_eq!(
            since("2026-09-16T12:30:00+01:00", now).unwrap(),
            now - Duration::hours(1)
        );
        for bad in [
            "",
            "0s",
            "-1h",
            "+1h",
            " 1h",
            "1h ",
            "1.5h",
            "1h30m",
            "1H",
            "999999999999999999999d",
            "🦀",
        ] {
            assert!(since(bad, now).is_err(), "{bad}");
        }
    }

    #[test]
    fn hourly_bounds_preserve_alignment_clamp_retention_and_expose_partial_hour() {
        let now = at("2026-09-16T12:30:00Z");
        let retained = at("2026-09-15T00:00:00Z");
        let range = Range::new(
            Some(now - Duration::minutes(10)),
            Some(now + Duration::hours(1)),
            now,
            retained,
        )
        .unwrap();
        assert_eq!(range.effective_since, at("2026-09-16T12:00:00Z"));
        assert_eq!(range.effective_until, at("2026-09-16T13:00:00Z"));
        assert!(range.future_until_clamped && range.includes_current_hour);
        let expired = Range::new(
            Some(retained - Duration::days(2)),
            Some(retained - Duration::days(1)),
            now,
            retained,
        )
        .unwrap();
        assert_eq!(expired.effective_since, retained);
        assert_eq!(expired.effective_until, retained);
        assert!(expired.history_truncated && !expired.includes_current_hour);
        let aligned = Range::new(Some(retained), Some(hour(now)), now, retained).unwrap();
        assert_eq!(aligned.effective_until, hour(now));
        assert!(!aligned.includes_current_hour);
        for (start, end) in [
            (now, now),
            (now, now - Duration::seconds(1)),
            (now + Duration::seconds(1), now + Duration::hours(1)),
        ] {
            assert!(Range::new(Some(start), Some(end), now, retained).is_err());
        }
    }
}
