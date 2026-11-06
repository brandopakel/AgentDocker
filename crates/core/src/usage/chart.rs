//! The buckets a usage chart draws: a report's hourly rows folded into
//! hours or days over its range. Every bucket of the range is present,
//! empty where nothing was reported (a gap, never a zero), and the bucket
//! the clock is in is marked as still filling. Pure: the report carries
//! the clock.

use super::report::{CounterReports, Group, Report};
use super::{CounterReport, Coverage, hour};
use chrono::{DateTime, Duration, Utc};

/// How long one bucket is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Hour,
    Day,
}

/// More buckets than this is no chart: a range that long is read by day.
pub const MOST_BUCKETS: usize = 1_000;

impl Step {
    /// Hours over a window of two days or less, days over a longer one.
    pub fn for_range(since: DateTime<Utc>, until: DateTime<Utc>) -> Self {
        if until - since <= Duration::hours(48) {
            Self::Hour
        } else {
            Self::Day
        }
    }

    /// The boundary at or before `at`: the hour, or the UTC midnight.
    pub fn floor(self, at: DateTime<Utc>) -> DateTime<Utc> {
        match self {
            Self::Hour => hour(at),
            Self::Day => at.date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc(),
        }
    }

    fn length(self) -> Duration {
        match self {
            Self::Hour => Duration::hours(1),
            Self::Day => Duration::days(1),
        }
    }
}

/// One bar of the chart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bucket {
    /// Clipped to the report's range: the first and last buckets of a
    /// range that does not start or end on a boundary are shorter.
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub samples: u64,
    /// `None` where no row fell in the bucket: nothing was reported,
    /// which is not a zero.
    pub counters: Option<CounterReports>,
    /// The bucket the report's clock is in, still filling as it is read.
    pub filling: bool,
}

impl Bucket {
    /// Input, cache read, cache write and output together, where samples
    /// said: the height of the bar. Reasoning sits inside output.
    pub fn moved(&self) -> Option<u64> {
        let counters = self.counters.as_ref()?;
        let parts = [
            &counters.input_tokens,
            &counters.cache_read_input_tokens,
            &counters.cache_write_input_tokens,
            &counters.output_tokens,
        ];
        if parts.iter().all(|c| c.sum.is_none()) {
            return None;
        }
        Some(
            parts
                .iter()
                .filter_map(|c| c.sum)
                .fold(0u64, u64::saturating_add),
        )
    }
}

/// The report's hour rows folded into buckets of `step` over its range,
/// in time order. Refused rather than guessed at: a report grouped by
/// anything but hour, a row without a parsable hour or outside the
/// range, or more buckets than [`MOST_BUCKETS`].
pub fn buckets(report: &Report, step: Step) -> Result<Vec<Bucket>, &'static str> {
    if report.by != Group::Hour {
        return Err("a usage chart is drawn from rows by hour");
    }
    let (since, until) = (report.effective_since, report.effective_until);
    if since >= until {
        return Ok(Vec::new());
    }
    let mut buckets = Vec::new();
    let mut start = since;
    while start < until {
        let next = step
            .floor(start)
            .checked_add_signed(step.length())
            .ok_or("usage chart range overflows")?;
        let end = next.min(until);
        buckets.push(Bucket {
            since: start,
            until: end,
            samples: 0,
            counters: None,
            filling: report.coverage.includes_current_hour
                && start <= report.as_of
                && report.as_of < end,
        });
        if buckets.len() > MOST_BUCKETS {
            return Err("usage chart range has too many buckets");
        }
        start = end;
    }
    for row in &report.rows {
        let at = row
            .key
            .as_deref()
            .and_then(|key| DateTime::parse_from_rfc3339(key).ok())
            .map(|at| at.with_timezone(&Utc))
            .ok_or("a usage row by hour has no hour")?;
        let bucket = buckets
            .iter_mut()
            .find(|b| b.since <= at && at < b.until)
            .ok_or("a usage row by hour is outside the report's range")?;
        bucket.samples = bucket.samples.saturating_add(row.samples);
        bucket.counters = Some(match bucket.counters.take() {
            None => row.counters.clone(),
            Some(so_far) => merge(&so_far, &row.counters),
        });
    }
    Ok(buckets)
}

/// Two rows' counters as one: the sum where either said, complete only
/// where both were, partial where one was not — never a zero for an
/// unknown.
pub fn merge(a: &CounterReports, b: &CounterReports) -> CounterReports {
    let one = |x: &CounterReport, y: &CounterReport| {
        let sum = match (x.sum, y.sum) {
            (None, None) => None,
            (Some(x), None) | (None, Some(x)) => Some(x),
            (Some(x), Some(y)) => Some(x.saturating_add(y)),
        };
        CounterReport {
            sum,
            known_samples: x.known_samples.saturating_add(y.known_samples),
            coverage: match sum {
                None => Coverage::Unknown,
                Some(_) if x.coverage == Coverage::Complete && y.coverage == Coverage::Complete => {
                    Coverage::Complete
                }
                Some(_) => Coverage::Partial,
            },
        }
    };
    CounterReports {
        input_tokens: one(&a.input_tokens, &b.input_tokens),
        cache_read_input_tokens: one(&a.cache_read_input_tokens, &b.cache_read_input_tokens),
        cache_write_input_tokens: one(&a.cache_write_input_tokens, &b.cache_write_input_tokens),
        output_tokens: one(&a.output_tokens, &b.output_tokens),
        reasoning_output_tokens: one(&a.reasoning_output_tokens, &b.reasoning_output_tokens),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::report::{Collection, Overhead, ReportCoverage, Row};

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn counter(sum: Option<u64>, coverage: Coverage) -> CounterReport {
        CounterReport {
            sum,
            known_samples: sum.map_or(0, |_| 1),
            coverage,
        }
    }

    fn row(key: &str, input: Option<u64>, input_coverage: Coverage, output: Option<u64>) -> Row {
        Row {
            key: Some(key.to_owned()),
            samples: 1,
            counters: CounterReports {
                input_tokens: counter(input, input_coverage),
                cache_read_input_tokens: counter(None, Coverage::Unknown),
                cache_write_input_tokens: counter(None, Coverage::Unknown),
                output_tokens: counter(output, Coverage::Complete),
                reasoning_output_tokens: counter(None, Coverage::Unknown),
            },
        }
    }

    fn report(rows: Vec<Row>, since: &str, until: &str, as_of: &str) -> Report {
        Report {
            rows,
            by: Group::Hour,
            as_of: at(as_of),
            effective_since: at(since),
            effective_until: at(until),
            coverage: ReportCoverage {
                retained_since: at("2026-03-01T00:00:00Z"),
                history_truncated: false,
                future_until_clamped: false,
                includes_current_hour: true,
                source_gaps: 0,
                tracking: None,
                collection: Collection::default(),
            },
            overhead: Overhead::default(),
        }
    }

    /// A day's window is read by hour, a week's by day; the boundary is
    /// the hour or the UTC midnight.
    #[test]
    fn a_step_follows_the_window_and_floors_to_its_boundary() {
        let now = at("2026-04-10T15:20:00Z");
        assert_eq!(Step::for_range(now - Duration::hours(24), now), Step::Hour);
        assert_eq!(Step::for_range(now - Duration::hours(48), now), Step::Hour);
        assert_eq!(Step::for_range(now - Duration::days(7), now), Step::Day);
        assert_eq!(Step::Hour.floor(now), at("2026-04-10T15:00:00Z"));
        assert_eq!(Step::Day.floor(now), at("2026-04-10T00:00:00Z"));
    }

    /// Every hour of the range is a bucket in order; one with no row is
    /// empty rather than zero; the hour the clock is in is filling.
    #[test]
    fn hours_fill_the_range_with_gaps_kept_as_gaps() {
        let read = report(
            vec![
                row(
                    "2026-04-10T13:00:00Z",
                    Some(10),
                    Coverage::Complete,
                    Some(1),
                ),
                row(
                    "2026-04-10T15:00:00Z",
                    Some(30),
                    Coverage::Complete,
                    Some(3),
                ),
            ],
            "2026-04-10T12:00:00Z",
            "2026-04-10T16:00:00Z",
            "2026-04-10T15:20:00Z",
        );
        let hours = buckets(&read, Step::Hour).unwrap();
        assert_eq!(hours.len(), 4);
        assert_eq!(hours[0].since, at("2026-04-10T12:00:00Z"));
        assert_eq!(hours[0].until, at("2026-04-10T13:00:00Z"));
        assert_eq!(hours[0].counters, None, "nothing reported is a gap");
        assert_eq!(hours[0].moved(), None);
        assert_eq!(hours[1].moved(), Some(11));
        assert_eq!(hours[2].counters, None);
        assert_eq!(hours[3].moved(), Some(33));
        assert_eq!(
            hours.iter().map(|b| b.filling).collect::<Vec<_>>(),
            [false, false, false, true]
        );
        // A range that ends on the hour has no filling bucket even when
        // the report says the clock is in one past it.
        let closed = report(
            Vec::new(),
            "2026-04-10T12:00:00Z",
            "2026-04-10T15:00:00Z",
            "2026-04-10T15:20:00Z",
        );
        assert!(
            buckets(&closed, Step::Hour)
                .unwrap()
                .iter()
                .all(|b| !b.filling)
        );
    }

    /// Days: the first bucket runs from the range's start to the next
    /// midnight, the rest are whole days, the last ends with the range;
    /// hours of one day fold into one bucket with the weaker coverage.
    #[test]
    fn days_fold_hours_and_keep_the_weakest_coverage() {
        let read = report(
            vec![
                row("2026-04-08T09:00:00Z", Some(5), Coverage::Complete, Some(1)),
                row(
                    "2026-04-09T01:00:00Z",
                    Some(100),
                    Coverage::Complete,
                    Some(2),
                ),
                row("2026-04-09T02:00:00Z", None, Coverage::Unknown, Some(4)),
                row("2026-04-09T03:00:00Z", Some(7), Coverage::Partial, Some(8)),
            ],
            "2026-04-08T09:00:00Z",
            "2026-04-10T16:00:00Z",
            "2026-04-10T15:20:00Z",
        );
        let days = buckets(&read, Step::Day).unwrap();
        assert_eq!(
            days.iter().map(|b| (b.since, b.until)).collect::<Vec<_>>(),
            [
                (at("2026-04-08T09:00:00Z"), at("2026-04-09T00:00:00Z")),
                (at("2026-04-09T00:00:00Z"), at("2026-04-10T00:00:00Z")),
                (at("2026-04-10T00:00:00Z"), at("2026-04-10T16:00:00Z")),
            ]
        );
        assert_eq!(days[0].samples, 1);
        let day = days[1].counters.as_ref().unwrap();
        assert_eq!(days[1].samples, 3);
        assert_eq!(day.input_tokens.sum, Some(107));
        assert_eq!(day.input_tokens.known_samples, 2);
        assert_eq!(
            day.input_tokens.coverage,
            Coverage::Partial,
            "an hour that did not say makes the day partial"
        );
        assert_eq!(day.output_tokens.sum, Some(14));
        assert_eq!(day.output_tokens.coverage, Coverage::Complete);
        assert_eq!(day.cache_read_input_tokens.sum, None);
        assert_eq!(day.cache_read_input_tokens.coverage, Coverage::Unknown);
        assert!(days[2].filling && days[2].counters.is_none());
    }

    /// Nothing is guessed: a report by anything but hour, a row without
    /// an hour, a row outside the range and an empty range.
    #[test]
    fn a_chart_refuses_what_it_cannot_place() {
        let mut read = report(
            vec![row(
                "2026-04-10T13:00:00Z",
                Some(1),
                Coverage::Complete,
                None,
            )],
            "2026-04-10T12:00:00Z",
            "2026-04-10T16:00:00Z",
            "2026-04-10T15:20:00Z",
        );
        assert!(buckets(&read, Step::Hour).is_ok());
        read.by = Group::Agent;
        assert!(buckets(&read, Step::Hour).is_err());
        read.by = Group::Hour;
        read.rows[0].key = None;
        assert!(buckets(&read, Step::Hour).is_err());
        read.rows[0].key = Some("2026-04-10T11:00:00Z".into());
        assert!(buckets(&read, Step::Hour).is_err(), "outside the range");
        read.rows.clear();
        read.effective_until = read.effective_since;
        assert!(buckets(&read, Step::Hour).unwrap().is_empty());
        read.effective_until = read.effective_since + Duration::days(60);
        assert!(buckets(&read, Step::Hour).is_err(), "too many buckets");
        assert_eq!(
            buckets(&read, Step::Day).unwrap().len(),
            61,
            "half a day, fifty-nine whole days, half a day"
        );
    }
}
