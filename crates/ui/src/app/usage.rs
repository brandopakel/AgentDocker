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

/// Only current configuration establishes that collection is off. Missing
/// discovery progress may mean it is starting or this is an older report.
pub fn collection_off(report: &Report) -> bool {
    report.coverage.collection.enabled == Some(false)
}

/// One line on where collection stands.
pub fn collection_line(report: &Report) -> String {
    let collection = &report.coverage.collection;
    let state = match collection.state {
        CollectionState::Unknown if collection_off(report) => "off".to_owned(),
        CollectionState::Unknown
            if collection.enabled == Some(true) && collection.discovery_generation.is_none() =>
        {
            "starting".to_owned()
        }
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
    if report
        .coverage
        .tracking
        .as_ref()
        .is_some_and(|t| t.capacity_gap)
    {
        line.push_str(" · tracking storage limit reached; some records were not counted");
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
        let compact = self.narrow() || self.panes.workspace_width() < 940.0;
        let filters: Element<'_, Message> = if compact {
            column![windows, groups].spacing(10).into()
        } else {
            row![windows, groups].spacing(10).align_y(Center).into()
        };
        let mut page = column![filters].spacing(14).width(Fill);
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
                    "Enable it in agentd.toml ([usage] enabled = true) to read the providers' local usage logs. Existing totals remain available.",
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
            page = page.push(self.usage_table(report, compact, c));
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
    fn usage_table(&self, report: &Report, compact: bool, c: Colors) -> Element<'_, Message> {
        let key = match report.by {
            Group::Agent => "Agent",
            Group::Model => "Model",
            Group::Provider => "Provider",
            Group::Project => "Project",
            Group::Hour => "Hour",
        };
        if compact {
            let mut cards = column![].spacing(10).width(Fill);
            for row_ in &report.rows {
                let mut card = column![
                    text(self.usage_key(row_))
                        .size(15)
                        .font(weight(iced::font::Weight::Medium)),
                    small(format!("{} samples", thousands(row_.samples)), c),
                ]
                .spacing(6)
                .width(Fill);
                for (label, value) in [
                    ("Input", &row_.counters.input_tokens),
                    ("Cache read", &row_.counters.cache_read_input_tokens),
                    ("Cache write", &row_.counters.cache_write_input_tokens),
                    ("Output", &row_.counters.output_tokens),
                    ("Reasoning", &row_.counters.reasoning_output_tokens),
                ] {
                    card = card.push(
                        row![
                            container(small(label, c)).width(Fill),
                            text(counter(value)).size(13).color(c.text),
                        ]
                        .spacing(12),
                    );
                }
                cards = cards.push(panel(card, c));
            }
            return cards
                .push(small("~ partial coverage · — not reported", c))
                .into();
        }
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
        table = table.push(small("~ partial coverage · — not reported", c));
        panel(table, c)
    }

    fn usage_key(&self, row_: &Row) -> String {
        match &row_.key {
            Some(key) if matches!(self.usage.as_ref().map(|(_, r)| r.by), Some(Group::Agent)) => {
                self.name_of(key)
            }
            Some(key) => key.clone(),
            None => "(unattributed)".to_owned(),
        }
    }

    fn usage_row(&self, row_: &Row, c: Colors) -> Element<'_, Message> {
        let key = self.usage_key(row_);
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
                tracking: None,
                collection: Collection {
                    enabled: Some(true),
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

    #[test]
    fn tracking_capacity_gap_is_visible_and_old_reports_still_decode() {
        let mut report = report(vec![], CollectionState::CaughtUp, Some(1));
        let old = serde_json::to_value(&report).unwrap();
        assert!(old["coverage"].get("tracking").is_none());
        let old: Report = serde_json::from_value(old).unwrap();
        assert!(old.coverage.tracking.is_none());
        assert!(!range_line(&old).contains("storage limit"));
        report.coverage.tracking = Some(agentdocker_core::usage::report::Tracking {
            logical_bytes: 1024,
            capacity_bytes: 1024,
            capacity_gap: true,
        });
        assert!(
            range_line(&report)
                .contains("tracking storage limit reached; some records were not counted")
        );
    }

    #[test]
    fn usage_reads_ignore_old_filters_and_report_queue_refusal_without_losing_totals() {
        let (tx, commands) = queue::channel();
        let (messages, rx) = std::sync::mpsc::sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(tx, rx);
        app.connected = Ok(());
        let dir = tempfile::tempdir().unwrap();
        let project = agentdocker_core::ProjectRef::directory(dir.path().join("project"));
        let root = project.root.display().to_string();
        app.shell.catalog.remember(project.clone(), true);
        app.shell.catalog.selected = Some(project.root);
        app.request_usage();
        app.request_usage();
        let first: Vec<_> = commands.try_iter().collect();
        assert_eq!(
            first.len(),
            1,
            "identical refreshes share one outstanding read"
        );
        let Cmd::Usage { request: old, .. } = &first[0] else {
            panic!("usage request")
        };
        app.shell.usage_since = "7d";
        app.request_usage();
        let Cmd::Usage {
            request: current, ..
        } = commands.try_iter().next().unwrap()
        else {
            panic!("usage request")
        };
        messages
            .send(Msg::Usage(root.clone(), *old, Err("old error".into())))
            .unwrap();
        app.drain();
        assert!(app.usage_error.is_none());
        let latest = report(Vec::new(), CollectionState::CaughtUp, Some(4));
        messages
            .send(Msg::Usage(root.clone(), current, Ok(latest.clone())))
            .unwrap();
        app.drain();
        assert_eq!(app.usage.as_ref().unwrap().1, latest);
        messages
            .send(Msg::Usage(
                root,
                *old,
                Ok(report(Vec::new(), CollectionState::Unknown, None)),
            ))
            .unwrap();
        app.drain();
        assert_eq!(app.usage.as_ref().unwrap().1, latest);
        for _ in 0..queue::CAPACITY {
            app.tx.send(Cmd::Stop("fixture".into())).unwrap();
        }
        app.request_usage();
        assert!(app.usage_pending.is_none());
        assert!(app.usage_error.is_some());
        assert_eq!(app.usage.as_ref().unwrap().1, latest);
    }

    /// The usage on view follows the project and the connection: another
    /// project selected while Usage is on view is read at once; a screen
    /// opened while the daemon was away asks when it is back; a read that
    /// was on its way when the daemon went is not waited for.
    #[test]
    fn usage_follows_the_project_on_view_and_the_connection() {
        let (tx, commands) = queue::channel();
        let (messages, rx) = std::sync::mpsc::sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(tx, rx);
        app.connected = Ok(());
        let dir = tempfile::tempdir().unwrap();
        let first = agentdocker_core::ProjectRef::directory(dir.path().join("first"));
        let second = agentdocker_core::ProjectRef::directory(dir.path().join("second"));
        app.shell.catalog.remember(first.clone(), true);
        app.shell.catalog.remember(second.clone(), true);
        app.shell.catalog.selected = Some(first.root.clone());
        app.screen = Screen::Usage;
        app.request_usage();
        let Some(Cmd::Usage { project, .. }) = commands.try_iter().next() else {
            panic!("the first project's usage is read")
        };
        assert_eq!(project, first.root.display().to_string());
        // Another project on view: its usage is read without a filter
        // being touched.
        app.shell.catalog.selected = Some(second.root.clone());
        app.refresh_project_context();
        let read: Vec<String> = commands
            .try_iter()
            .filter_map(|cmd| match cmd {
                Cmd::Usage { project, .. } => Some(project),
                _ => None,
            })
            .collect();
        assert_eq!(
            read,
            vec![second.root.display().to_string()],
            "the second project's usage is read on selection, once"
        );
        // The daemon goes while that read is on its way: the read is not
        // waited for, and the reconnect asks again for the screen on view.
        messages.send(Msg::Disconnected("gone".into())).unwrap();
        app.drain();
        assert!(app.usage_pending.is_none());
        messages.send(Msg::Connected).unwrap();
        app.drain();
        assert!(
            commands
                .try_iter()
                .any(|cmd| matches!(cmd, Cmd::Usage { ref project, .. } if *project == second.root.display().to_string())),
            "asked again on reconnect"
        );
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

        let mut off = report(Vec::new(), CollectionState::Unknown, None);
        assert!(!collection_off(&off));
        assert_eq!(collection_line(&off), "Collection: starting");
        off.coverage.collection.enabled = None;
        assert!(!collection_off(&off));
        assert_eq!(collection_line(&off), "Collection: unknown");
        off.coverage.collection.enabled = Some(false);
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
