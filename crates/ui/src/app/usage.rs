//! Usage: the tokens the providers reported for the project's sessions,
//! and — beside every total — what it covers. A count is shown only
//! where samples said; the report's own range, retention, gaps and
//! collection state sit under the table, and the overhead AgentDocker
//! injected is "not measured" until it is, never a zero.
use super::style::{Colors, weight};
use super::view::{empty, eyebrow, note, panel, rule, segmented, small};
use super::*;
use crate::controls::segment;
use agentdocker_core::usage::report::{CollectionState, Group, Report, Row};
use agentdocker_core::usage::{CounterReport, Coverage};
use iced::{
    Center, Element, Fill,
    widget::{column, container, row, text},
};

/// The window shown until the person picks another.
pub const DEFAULT_SINCE: &str = "24h";
/// The windows on offer: what a person asks about a day, a week, a
/// month — the daemon keeps thirty days by default.
pub const WINDOWS: [(&str, &str); 3] = [
    ("24h", "Last 24 hours"),
    ("7d", "Last 7 days"),
    ("30d", "Last 30 days"),
];

/// A count as the report knows it: the sum where every sample said, the
/// sum marked `~` where some did not, `—` where none did.
pub fn counter(report: &CounterReport) -> String {
    match (report.sum, report.coverage) {
        (None, _) | (_, Coverage::Unknown) => "—".to_owned(),
        (Some(sum), Coverage::Complete) => thousands(sum),
        (Some(sum), Coverage::Partial) => format!("{}~", thousands(sum)),
    }
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

/// Whether collection has never run: the state is unknown and no
/// discovery has been numbered. That is "off" (the default), not "no
/// usage".
pub fn collection_off(report: &Report) -> bool {
    let collection = &report.coverage.collection;
    collection.state == CollectionState::Unknown && collection.discovery_generation.is_none()
}

/// One line on where collection stands.
pub fn collection_line(report: &Report) -> String {
    let collection = &report.coverage.collection;
    let state = match collection.state {
        CollectionState::Unknown if collection_off(report) => "off".to_owned(),
        CollectionState::Unknown => "unknown".to_owned(),
        CollectionState::Scanning => match collection.pending_files {
            Some(pending) => format!("scanning, {pending} file(s) to read"),
            None => "scanning".to_owned(),
        },
        CollectionState::CaughtUp => match collection.completed_at {
            Some(at) => format!(
                "caught up {}",
                agentdocker_core::journal::ago(chrono::Utc::now(), at)
            ),
            None => "caught up".to_owned(),
        },
    };
    let roots = collection.scope.roots.len();
    if roots == 0 {
        format!("Collection: {state}")
    } else {
        format!("Collection: {state} · {roots} root(s)")
    }
}

/// One line on the overhead: measured, bytes only, or not yet.
pub fn overhead_line(report: &Report) -> String {
    let overhead = &report.overhead;
    match (&overhead.estimated_tokens, overhead.injected_bytes) {
        (Some(estimate), _) => format!(
            "Overhead: about {} tokens injected by AgentDocker, from {} known event(s)",
            thousands(estimate.value),
            overhead.known_events
        ),
        (None, Some(bytes)) => format!(
            "Overhead: {} bytes injected by AgentDocker over {} known event(s); tokens not estimated",
            thousands(bytes),
            overhead.known_events
        ),
        (None, None) => "Overhead: not measured yet".to_owned(),
    }
}

/// One line on the range the totals cover and what may be missing.
pub fn range_line(report: &Report) -> String {
    let mut line = format!(
        "{} to {}",
        report.effective_since.format("%b %-d %H:%M"),
        report.effective_until.format("%b %-d %H:%M")
    );
    if report.coverage.includes_current_hour {
        line.push_str(" · the current hour is still filling");
    }
    if report.coverage.history_truncated {
        line.push_str(&format!(
            " · retained since {}",
            report.coverage.retained_since.format("%b %-d")
        ));
    }
    if report.coverage.source_gaps > 0 {
        line.push_str(&format!(
            " · {} gap(s) in the sources: lower bounds",
            report.coverage.source_gaps
        ));
    }
    line
}

impl App {
    pub(super) fn usage_view(&self, c: Colors) -> Element<'_, Message> {
        let Some(project) = self.selected_project_root() else {
            return empty(
                "Pick a project",
                "Usage is a project's: choose one on the left.",
                None,
                c,
            );
        };
        let since = if self.shell.usage_since.is_empty() {
            DEFAULT_SINCE
        } else {
            self.shell.usage_since
        };
        let windows = segmented(
            WINDOWS
                .iter()
                .map(|(key, label)| {
                    segment(
                        format!("usage-since-{key}"),
                        *label,
                        Some(Message::UsageSince(key)),
                        since == *key,
                    )
                })
                .collect(),
            c,
        );
        let groups = segmented(
            [
                (Group::Agent, "By agent"),
                (Group::Model, "By model"),
                (Group::Provider, "By provider"),
                (Group::Hour, "By hour"),
            ]
            .into_iter()
            .map(|(group, label)| {
                segment(
                    format!("usage-by-{group:?}"),
                    label,
                    Some(Message::UsageBy(group)),
                    self.shell.usage_by == group,
                )
            })
            .collect(),
            c,
        );
        let mut page = column![row![windows, groups].spacing(10).align_y(Center)]
            .spacing(14)
            .width(Fill);
        if let Some(error) = &self.usage_error {
            page = page.push(note(format!("Could not read usage: {error}"), c).color(c.amber));
        }
        let report = self
            .usage
            .as_ref()
            .filter(|(p, _)| *p == project)
            .map(|(_, r)| r);
        let Some(report) = report else {
            if self.usage_error.is_none() {
                page = page.push(note("Reading…", c));
            }
            return page.into();
        };
        if report.rows.is_empty() {
            page = page.push(if collection_off(report) {
                empty(
                    "Collection is off",
                    "Nothing has been read. Enable it in agentd.toml ([usage] enabled = true) and the daemon reads the providers' local logs from then on.",
                    None,
                    c,
                )
            } else {
                column![
                    text("No usage in this window")
                        .size(15)
                        .font(weight(iced::font::Weight::Medium)),
                    note(range_line(report), c),
                ]
                .spacing(4)
                .into()
            });
        } else {
            page = page.push(self.usage_table(report, c));
        }
        page = page.push(
            column![
                small(range_line(report), c),
                small(collection_line(report), c),
                small(overhead_line(report), c),
            ]
            .spacing(4),
        );
        page.into()
    }

    /// The rows as a table: the key, how many samples, then each count
    /// with its coverage mark; a legend under it.
    fn usage_table(&self, report: &Report, c: Colors) -> Element<'_, Message> {
        let key = match report.by {
            Group::Agent => "Agent",
            Group::Model => "Model",
            Group::Provider => "Provider",
            Group::Project => "Project",
            Group::Hour => "Hour",
        };
        let head = |label: &str| eyebrow(label.to_owned(), c);
        let mut table = column![
            row![
                container(head(key)).width(Fill),
                container(head("Samples")).width(90),
                container(head("Input")).width(110),
                container(head("Cache read")).width(110),
                container(head("Cache write")).width(110),
                container(head("Output")).width(110),
                container(head("Reasoning")).width(110),
            ]
            .spacing(8),
            rule(c),
        ]
        .spacing(6)
        .width(Fill);
        for row_ in &report.rows {
            table = table.push(self.usage_row(row_, c));
        }
        table = table.push(small("~ some samples did not say · — none did", c));
        panel(table, c)
    }

    fn usage_row(&self, row_: &Row, c: Colors) -> Element<'_, Message> {
        let key = match &row_.key {
            Some(key) if matches!(self.usage.as_ref().map(|(_, r)| r.by), Some(Group::Agent)) => {
                self.name_of(key)
            }
            Some(key) => key.clone(),
            None => "(unattributed)".to_owned(),
        };
        let cell = |value: String| {
            container(text(value).size(13).color(c.text))
                .width(110)
                .align_x(iced::alignment::Horizontal::Right)
        };
        row![
            container(text(key).size(13).font(weight(iced::font::Weight::Medium))).width(Fill),
            container(text(thousands(row_.samples)).size(13).color(c.muted))
                .width(90)
                .align_x(iced::alignment::Horizontal::Right),
            cell(counter(&row_.counters.input_tokens)),
            cell(counter(&row_.counters.cache_read_input_tokens)),
            cell(counter(&row_.counters.cache_write_input_tokens)),
            cell(counter(&row_.counters.output_tokens)),
            cell(counter(&row_.counters.reasoning_output_tokens)),
        ]
        .spacing(8)
        .align_y(Center)
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::usage::report::{Collection, Overhead, ReportCoverage, Scope};

    fn report(rows: Vec<Row>, state: CollectionState, generation: Option<u64>) -> Report {
        let now = chrono::Utc::now();
        Report {
            rows,
            by: Group::Agent,
            as_of: now,
            effective_since: now - chrono::Duration::hours(24),
            effective_until: now,
            coverage: ReportCoverage {
                retained_since: now - chrono::Duration::days(30),
                history_truncated: false,
                future_until_clamped: false,
                includes_current_hour: true,
                source_gaps: 0,
                collection: Collection {
                    state,
                    discovery_generation: generation,
                    snapshot_at: None,
                    completed_at: None,
                    discovery_complete: false,
                    pending_files: None,
                    pending_tail_files: None,
                    scope: Scope::default(),
                },
            },
            overhead: Overhead::default(),
        }
    }

    /// A count shows as the report knows it and never as an invented
    /// zero; overhead that was not measured says so; collection that
    /// never ran is off, not "no usage".
    #[test]
    fn counts_carry_their_coverage_and_nothing_is_invented() {
        let report_for = |sum: Option<u64>, coverage| CounterReport {
            sum,
            known_samples: sum.map_or(0, |_| 1),
            coverage,
        };
        assert_eq!(
            counter(&report_for(Some(1_234_567), Coverage::Complete)),
            "1,234,567"
        );
        assert_eq!(counter(&report_for(Some(900), Coverage::Partial)), "900~");
        assert_eq!(counter(&report_for(None, Coverage::Unknown)), "—");
        assert_eq!(counter(&report_for(Some(0), Coverage::Unknown)), "—");
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(10_000_000), "10,000,000");

        let off = report(Vec::new(), CollectionState::Unknown, None);
        assert!(collection_off(&off));
        assert_eq!(collection_line(&off), "Collection: off");
        assert_eq!(overhead_line(&off), "Overhead: not measured yet");
        let scanning = report(Vec::new(), CollectionState::Scanning, Some(3));
        assert!(!collection_off(&scanning));
        assert_eq!(collection_line(&scanning), "Collection: scanning");
        let mut measured = report(Vec::new(), CollectionState::CaughtUp, Some(4));
        measured.overhead.injected_bytes = Some(2_048);
        measured.overhead.known_events = 7;
        assert_eq!(
            overhead_line(&measured),
            "Overhead: 2,048 bytes injected by AgentDocker over 7 known event(s); tokens not estimated"
        );
        assert!(range_line(&measured).contains("the current hour is still filling"));
        measured.coverage.source_gaps = 2;
        assert!(range_line(&measured).contains("2 gap(s)"));
    }
}
