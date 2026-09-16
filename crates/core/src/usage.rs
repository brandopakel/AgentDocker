//! Token accounting without provider I/O or an implicit wall clock.
//!
//! Input/output totals include their reported cache/reasoning components.
//! Unknown counters stay optional; a known zero is not missing data.

use chrono::{DateTime, Duration, Timelike, Utc};
use serde::{Deserialize, Serialize};

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Total {
    sum: u64,
    known_samples: u64,
}

/// A bucket retains known/sample counts with the sums, so moving historical
/// attribution never turns unknown fields into zero or loses coverage.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Aggregate {
    samples: u64,
    totals: [Total; 5],
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

impl Aggregate {
    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// Return a proposed replacement; an overflow/invalid removal cannot
    /// partially modify an existing bucket. Persistence commits it separately.
    pub fn add(&self, counters: &Counters) -> Result<Self, &'static str> {
        self.change(counters, false)
    }

    pub fn remove(&self, counters: &Counters) -> Result<Self, &'static str> {
        self.change(counters, true)
    }

    fn change(&self, counters: &Counters, remove: bool) -> Result<Self, &'static str> {
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
        for (total, value) in next.totals.iter_mut().zip(counters.values()) {
            if let Some(value) = value {
                total.sum = change(total.sum, value)?;
                total.known_samples = change(total.known_samples, 1)?;
            }
            if total.known_samples > next.samples || (total.known_samples == 0 && total.sum != 0) {
                return Err("usage bucket coverage is inconsistent");
            }
        }
        Ok(next)
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

#[cfg(test)]
mod tests {
    use super::*;
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
    fn aggregate_moves_unknowns_and_known_zero_with_their_contributions() {
        let zero = Counters::from_values([Some(0), None, None, Some(0), None]);
        let known = Counters::from_values([Some(10), Some(2), None, Some(3), Some(1)]);
        let original = Aggregate::default()
            .add(&zero)
            .unwrap()
            .add(&known)
            .unwrap();
        assert_eq!(original.samples(), 2);
        let complete = original.counters(true);
        assert_eq!(complete[0].sum, Some(10));
        assert_eq!(complete[0].coverage, Coverage::Complete);
        assert_eq!(complete[1].coverage, Coverage::Partial);
        assert_eq!(complete[2].coverage, Coverage::Unknown);
        assert_eq!(complete[2].sum, None);
        assert_eq!(original.counters(false)[0].coverage, Coverage::Partial);
        let source = original.remove(&known).unwrap();
        let destination = Aggregate::default().add(&known).unwrap();
        assert_eq!(source.counters(true)[0].sum, Some(0));
        assert_eq!(source.counters(true)[1].sum, None);
        assert_eq!(source.samples() + destination.samples(), original.samples());
        assert_eq!(source.add(&known).unwrap(), original);
        assert_eq!(source.remove(&zero).unwrap(), Aggregate::default());
    }

    #[test]
    fn rejected_bucket_arithmetic_preserves_original_sums_and_coverage() {
        let huge = Counters {
            input_tokens: Some(u64::MAX),
            ..Counters::default()
        };
        let original = Aggregate::default().add(&huge).unwrap();
        let saved = original.clone();
        assert!(original.add(&huge).is_err());
        assert!(original.remove(&Counters::default()).is_err());
        assert_eq!(original, saved);
        assert_eq!(original.remove(&huge).unwrap(), Aggregate::default());
        assert!(Aggregate::default().remove(&huge).is_err());
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
