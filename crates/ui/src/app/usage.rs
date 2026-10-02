//! Usage: the tokens the providers reported for the project's sessions,
//! and — beside every total — what it covers. A count is shown only
//! where samples said; the report's own range, retention, gaps and
//! collection state sit under the table, and the overhead AgentDocker
//! injected is "not measured" until it is, never a zero.
use super::icons::{Icon, icon};
use super::style::{Colors, weight};
use super::view::{empty, eyebrow, heading, icon_tile, monogram, note, rule, segmented, small};
use super::*;
use crate::controls::{Kind, custom};
use agentdocker_core::usage::report::{CollectionState, CounterReports, Group, Report, Row};
use agentdocker_core::usage::{CounterReport, Coverage};
use iced::{
    Center, Element, Fill,
    widget::{Space, column, container, row, text},
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

/// One counter summed over every row, with the coverage the rows give it
/// together: complete only where every row was, unknown where none said.
pub fn total(report: &Report, pick: Pick) -> CounterReport {
    let mut sum = 0u64;
    let mut known_samples = 0u64;
    let mut said = false;
    let mut complete = true;
    for row_ in &report.rows {
        let counter = pick(&row_.counters);
        if let Some(value) = counter.sum {
            sum = sum.saturating_add(value);
            said = true;
        }
        known_samples = known_samples.saturating_add(counter.known_samples);
        complete &= counter.coverage == Coverage::Complete;
    }
    CounterReport {
        sum: said.then_some(sum),
        known_samples,
        coverage: match (said, complete) {
            (false, _) => Coverage::Unknown,
            (true, true) => Coverage::Complete,
            (true, false) => Coverage::Partial,
        },
    }
}

/// A large count in three figures: `9,999`, `12.3K`, `1.23M`. Exact
/// below ten thousand; the table beside it keeps every digit.
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

/// [`counter`] in three figures, with the same coverage marks.
pub fn compact_counter(report: &CounterReport) -> String {
    match (report.sum, report.coverage) {
        (None, _) | (_, Coverage::Unknown) => "—".to_owned(),
        (Some(sum), Coverage::Complete) => compact(sum),
        (Some(sum), Coverage::Partial) => format!("{}~", compact(sum)),
    }
}

/// The tokens a row moved, every kind but reasoning (which providers
/// count inside output): the length of its share bar. Only what samples
/// said is counted.
fn moved(counters: &CounterReports) -> u64 {
    [
        &counters.input_tokens,
        &counters.cache_read_input_tokens,
        &counters.cache_write_input_tokens,
        &counters.output_tokens,
    ]
    .iter()
    .filter_map(|c| c.sum)
    .fold(0u64, u64::saturating_add)
}

/// The rows largest first — what a person asks of agents, models and
/// providers — except hours, which read in time order as reported.
fn ranked(report: &Report) -> Vec<&Row> {
    let mut rows: Vec<&Row> = report.rows.iter().collect();
    if report.by != Group::Hour {
        rows.sort_by_key(|r| std::cmp::Reverse(moved(&r.counters)));
    }
    rows
}

/// A share in whole percent; a share too small to round to one is said
/// as less than one rather than as nothing, and one short of the whole
/// as more than ninety-nine rather than as all of it.
fn percent(fraction: f32) -> String {
    let whole = (fraction * 100.0).round();
    if whole < 1.0 && fraction > 0.0 {
        "<1%".to_owned()
    } else if whole >= 100.0 && fraction < 1.0 {
        ">99%".to_owned()
    } else {
        format!("{whole:.0}%")
    }
}

/// Which of a row's counters a column reads.
type Pick = fn(&CounterReports) -> &CounterReport;

/// The five counters as the table and the cards name them.
const COUNTERS: [(&str, Pick); 5] = [
    ("Input", |c| &c.input_tokens),
    ("Cache read", |c| &c.cache_read_input_tokens),
    ("Cache write", |c| &c.cache_write_input_tokens),
    ("Output", |c| &c.output_tokens),
    ("Reasoning", |c| &c.reasoning_output_tokens),
];

/// What a group's rows are called, one and many.
fn group_noun(by: Group) -> (&'static str, &'static str) {
    match by {
        Group::Agent => ("agent", "agents"),
        Group::Model => ("model", "models"),
        Group::Provider => ("provider", "providers"),
        Group::Project => ("project", "projects"),
        Group::Hour => ("hour", "hours"),
    }
}

/// A numeric column's width in the table.
const NUMBER: f32 = 96.0;
/// The samples column: counts of records, never as long as tokens.
const SAMPLES: f32 = 72.0;
/// The workspace the share bars need beside the numbers.
const BARS_FIT: f32 = 1180.0;

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
        // The tracks say the choice in a word; each control still says
        // the whole of it to a screen reader.
        let windows = segmented(
            WINDOWS
                .iter()
                .map(|(key, label)| {
                    choice(
                        format!("usage-since-{key}"),
                        label,
                        match *key {
                            "24h" => "24 hours",
                            "7d" => "7 days",
                            _ => "30 days",
                        },
                        Message::UsageSince(key),
                        since == *key,
                    )
                })
                .collect(),
            c,
        );
        let groups = segmented(
            [
                (Group::Agent, "By agent", "Agent"),
                (Group::Model, "By model", "Model"),
                (Group::Provider, "By provider", "Provider"),
                (Group::Hour, "By hour", "Hour"),
            ]
            .into_iter()
            .map(|(group, label, word)| {
                choice(
                    format!("usage-by-{group:?}"),
                    label,
                    word,
                    Message::UsageBy(group),
                    self.shell.usage_by == group,
                )
            })
            .collect(),
            c,
        );
        let width = self.panes.workspace_width();
        let compact = self.narrow() || width < 940.0;
        let report = self
            .usage
            .as_ref()
            .filter(|(p, _)| *p == project)
            .map(|(_, r)| r);
        let window = WINDOWS
            .iter()
            .find(|(key, _)| *key == since)
            .map_or("Last 24 hours", |(_, label)| *label);
        let summary = match report {
            Some(report) if !report.rows.is_empty() => {
                let (one, many) = group_noun(report.by);
                let n = report.rows.len();
                format!("{window} · {n} {}", if n == 1 { one } else { many })
            }
            _ => window.to_owned(),
        };
        let title = column![
            heading("Usage", 18),
            note(format!("{summary} · tokens the providers reported"), c)
        ]
        .spacing(2);
        let header: Element<'_, Message> = if compact {
            column![title, row![windows, groups].spacing(8).wrap()]
                .spacing(12)
                .into()
        } else {
            row![title.width(Fill), windows, groups]
                .spacing(10)
                .align_y(Center)
                .into()
        };
        let mut page = column![header].spacing(16).width(Fill);
        if let Some(error) = &self.usage_error {
            page = page.push(note(format!("Could not read usage: {error}"), c).color(c.amber));
        }
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
                // The range it covers is said once, with the notes below.
                text("No usage in this window")
                    .size(15)
                    .font(weight(iced::font::Weight::Medium))
                    .into()
            });
        } else {
            page = page.push(kpis(report, compact, c)).push(self.usage_table(
                report,
                compact,
                width >= BARS_FIT,
                c,
            ));
        }
        page = page.push(
            column![
                small(range_line(report), c).color(c.faint),
                small(collection_line(report), c).color(c.faint),
                small(overhead_line(report), c).color(c.faint),
            ]
            .spacing(4),
        );
        page.into()
    }

    /// The rows: wide, one framed table — the key, its share of the
    /// tokens, how many samples, then each count with its coverage mark,
    /// numbers to the right; narrow, the same rows as entries in one card,
    /// every count still named and marked.
    fn usage_table(
        &self,
        report: &Report,
        compact: bool,
        bars: bool,
        c: Colors,
    ) -> Element<'_, Message> {
        let legend = small("~ partial coverage · — not reported", c).color(c.faint);
        let rows = ranked(report);
        if compact {
            let mut entries = column![].width(Fill);
            for (index, row_) in rows.iter().copied().enumerate() {
                if index > 0 {
                    entries = entries.push(rule(c));
                }
                let mut entry = column![
                    row![
                        self.usage_identity(row_, report.by, c),
                        text(self.usage_key(row_))
                            .size(14)
                            .font(weight(iced::font::Weight::Medium))
                            .width(Fill)
                            .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                        small(format!("{} samples", thousands(row_.samples)), c),
                    ]
                    .spacing(10)
                    .align_y(Center)
                ]
                .spacing(6)
                .width(Fill);
                // Every count named and marked, as many to a line as fit.
                let mut counts = row![].spacing(0);
                for (label, pick) in COUNTERS {
                    counts = counts.push(
                        column![
                            small(label, c),
                            text(counter(pick(&row_.counters))).size(13).color(c.text),
                        ]
                        .spacing(2)
                        .width(124)
                        .padding(iced::Padding {
                            bottom: 6.0,
                            ..iced::Padding::ZERO
                        }),
                    );
                }
                entry = entry.push(counts.wrap());
                entries = entries.push(container(entry).padding([12, 14]));
            }
            return column![
                container(entries)
                    .width(Fill)
                    .style(move |_| c.card_style()),
                legend
            ]
            .spacing(8)
            .into();
        }
        let (_, many) = group_noun(report.by);
        let head = |label: &str, width: f32| {
            container(eyebrow(label.to_owned(), c))
                .width(width)
                .align_x(iced::alignment::Horizontal::Right)
        };
        let mut header = row![container(eyebrow(many, c)).width(iced::Length::FillPortion(3))]
            .spacing(12)
            .align_y(Center);
        if bars {
            header =
                header.push(container(eyebrow("Share", c)).width(iced::Length::FillPortion(2)));
        }
        header = header.push(head("Samples", SAMPLES));
        for (label, _) in COUNTERS {
            header = header.push(head(label, NUMBER));
        }
        let all: u64 = report
            .rows
            .iter()
            .map(|r| moved(&r.counters))
            .fold(0, u64::saturating_add);
        let mut table = column![container(header).padding([10, 0]), rule(c)].width(Fill);
        for (index, row_) in rows.iter().copied().enumerate() {
            if index > 0 {
                table = table.push(rule(c));
            }
            table = table.push(
                container(self.usage_row(row_, report.by, bars.then_some(all), c)).padding([9, 0]),
            );
        }
        column![
            container(table)
                .padding([2, 16])
                .width(Fill)
                .style(move |_| c.card_style()),
            legend
        ]
        .spacing(8)
        .into()
    }

    fn usage_key(&self, row_: &Row) -> String {
        match &row_.key {
            Some(key) if matches!(self.usage.as_ref().map(|(_, r)| r.by), Some(Group::Agent)) => {
                self.name_of(key)
            }
            Some(key) if matches!(self.usage.as_ref().map(|(_, r)| r.by), Some(Group::Hour)) => {
                chrono::DateTime::parse_from_rfc3339(key).map_or_else(
                    |_| key.clone(),
                    |at| {
                        at.with_timezone(&chrono::Utc)
                            .format("%b %-d %H:%M")
                            .to_string()
                    },
                )
            }
            Some(key) => key.clone(),
            None => "(unattributed)".to_owned(),
        }
    }

    /// Who or what a row is, at a glance: an agent's own monogram, a
    /// neutral letter for a model or provider, a clock for an hour.
    fn usage_identity(&self, row_: &Row, by: Group, c: Colors) -> Element<'_, Message> {
        const SIZE: f32 = 22.0;
        let Some(key) = &row_.key else {
            return icon_tile(icon(Icon::Question, c.faint, 12.0), SIZE, c);
        };
        match by {
            Group::Agent => monogram(&self.name_of(key), key, SIZE, c),
            Group::Hour => icon_tile(icon(Icon::Clock, c.muted, 12.0), SIZE, c),
            _ => {
                let letter: String = key
                    .chars()
                    .find(|ch| ch.is_alphanumeric())
                    .map(|ch| ch.to_uppercase().collect())
                    .unwrap_or_else(|| "·".to_owned());
                icon_tile(
                    text(letter)
                        .size(11)
                        .font(weight(iced::font::Weight::Semibold))
                        .color(c.muted),
                    SIZE,
                    c,
                )
            }
        }
    }

    fn usage_row(
        &self,
        row_: &Row,
        by: Group,
        share_of: Option<u64>,
        c: Colors,
    ) -> Element<'_, Message> {
        let cell = |value: String, width: f32| {
            container(text(value).size(13).color(c.text))
                .width(width)
                .align_x(iced::alignment::Horizontal::Right)
        };
        let mut line = row![
            row![
                self.usage_identity(row_, by, c),
                container(
                    text(self.usage_key(row_))
                        .size(13)
                        .font(weight(iced::font::Weight::Medium))
                        .wrapping(iced::widget::text::Wrapping::None)
                )
                .width(Fill)
                .clip(true),
            ]
            .spacing(10)
            .align_y(Center)
            .width(iced::Length::FillPortion(3))
        ]
        .spacing(12)
        .align_y(Center);
        if let Some(all) = share_of {
            let part = moved(&row_.counters);
            let fraction = if all == 0 {
                0.0
            } else {
                (part as f64 / all as f64) as f32
            };
            line = line.push(
                row![
                    share_bar(fraction, c),
                    container(text(percent(fraction)).size(12).color(c.muted))
                        .width(36)
                        .align_x(iced::alignment::Horizontal::Right),
                ]
                .spacing(8)
                .align_y(Center)
                .width(iced::Length::FillPortion(2)),
            );
        }
        line = line.push(
            container(text(thousands(row_.samples)).size(13).color(c.muted))
                .width(SAMPLES)
                .align_x(iced::alignment::Horizontal::Right),
        );
        for (_, pick) in COUNTERS {
            line = line.push(cell(counter(pick(&row_.counters)), NUMBER));
        }
        line.into()
    }
}

/// A segmented choice whose visible word is shorter than what it says to
/// a screen reader.
fn choice<'a>(
    id: String,
    spoken: &str,
    word: &str,
    message: Message,
    selected: bool,
) -> Element<'a, Message> {
    custom(
        id,
        spoken.to_owned(),
        text(word.to_owned())
            .size(13)
            .font(weight(iced::font::Weight::Medium)),
        Some(message),
        selected,
        Kind::Segment,
        [5, 12],
    )
}

/// The headline counts over every row: input, cache read and write and
/// output, each in three figures with its coverage mark. Two by two when
/// the table has become cards.
fn kpis<'a>(report: &Report, compact: bool, c: Colors) -> Element<'a, Message> {
    let tile = |label: &str, value: String| -> Element<'a, Message> {
        container(
            column![
                text(label.to_owned())
                    .size(12)
                    .font(weight(iced::font::Weight::Medium))
                    .color(c.muted),
                text(value)
                    .size(22)
                    .font(weight(iced::font::Weight::Semibold))
                    .color(c.text),
            ]
            .spacing(6),
        )
        .padding([12, 14])
        .width(Fill)
        .style(move |_| c.card_style())
        .into()
    };
    let tiles: Vec<Element<'a, Message>> = COUNTERS[..4]
        .iter()
        .map(|(label, pick)| tile(label, compact_counter(&total(report, *pick))))
        .collect();
    if compact {
        let mut tiles = tiles.into_iter();
        let mut grid = column![].spacing(10).width(Fill);
        while let Some(first) = tiles.next() {
            let mut line = row![first].spacing(10).width(Fill);
            if let Some(second) = tiles.next() {
                line = line.push(second);
            }
            grid = grid.push(line);
        }
        grid.into()
    } else {
        let mut line = row![].spacing(12).width(Fill);
        for tile in tiles {
            line = line.push(tile);
        }
        line.into()
    }
}

/// A row's share of the tokens as a bar on a quiet track: one hue for
/// every row, the number beside it.
fn share_bar<'a>(fraction: f32, c: Colors) -> Element<'a, Message> {
    let filled = (fraction.clamp(0.0, 1.0) * 1000.0).round() as u16;
    let mut bar = row![].spacing(0);
    if filled > 0 {
        bar = bar.push(
            container(Space::new().width(Fill).height(8))
                .width(iced::Length::FillPortion(filled))
                .style(move |_| container::Style {
                    background: Some(super::style::mix(c.card, c.accent, 0.7).into()),
                    border: iced::Border {
                        radius: 2.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
        );
    }
    if filled < 1000 {
        bar = bar.push(Space::new().width(iced::Length::FillPortion(1000 - filled)));
    }
    container(bar)
        .width(Fill)
        .style(move |_| container::Style {
            background: Some(super::style::alpha(c.text, 0.06).into()),
            border: iced::Border {
                radius: 2.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
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

    /// The headline counts sum what the rows said and carry the weakest
    /// coverage among them; three figures never round past their unit.
    #[test]
    fn headline_counts_carry_the_rows_coverage_in_three_figures() {
        let counter = |sum: Option<u64>, coverage| CounterReport {
            sum,
            known_samples: sum.map_or(0, |_| 1),
            coverage,
        };
        let row_with = |input: CounterReport| Row {
            key: Some("k".into()),
            samples: 1,
            counters: CounterReports {
                input_tokens: input,
                cache_read_input_tokens: counter(None, Coverage::Unknown),
                cache_write_input_tokens: counter(None, Coverage::Unknown),
                output_tokens: counter(Some(5), Coverage::Complete),
                reasoning_output_tokens: counter(None, Coverage::Unknown),
            },
        };
        let mut both = report(
            vec![
                row_with(counter(Some(12_000), Coverage::Complete)),
                row_with(counter(Some(400), Coverage::Partial)),
            ],
            CollectionState::CaughtUp,
            Some(1),
        );
        let input = total(&both, |c| &c.input_tokens);
        assert_eq!(
            (input.sum, input.coverage),
            (Some(12_400), Coverage::Partial)
        );
        assert_eq!(compact_counter(&input), "12.4K~");
        let output = total(&both, |c| &c.output_tokens);
        assert_eq!(compact_counter(&output), "10");
        assert_eq!(
            compact_counter(&total(&both, |c| &c.cache_read_input_tokens)),
            "—"
        );
        both.rows.clear();
        assert_eq!(compact_counter(&total(&both, |c| &c.input_tokens)), "—");
        assert_eq!(compact(9_999), "9,999");
        assert_eq!(compact(12_345), "12.3K");
        assert_eq!(compact(123_456), "123K");
        assert_eq!(compact(999_950), "1.00M");
        assert_eq!(compact(1_234_567), "1.23M");
        assert_eq!(compact(45_600_000_000), "45.6B");
        assert_eq!(percent(0.0), "0%");
        assert_eq!(percent(0.004), "<1%");
        assert_eq!(percent(0.916), "92%");
        assert_eq!(percent(0.9999), ">99%");
        assert_eq!(percent(1.0), "100%");
        let small_first = report(
            vec![
                row_with(counter(Some(10), Coverage::Complete)),
                row_with(counter(Some(9_000), Coverage::Complete)),
            ],
            CollectionState::CaughtUp,
            Some(1),
        );
        let order: Vec<_> = ranked(&small_first)
            .iter()
            .map(|r| r.counters.input_tokens.sum)
            .collect();
        assert_eq!(order, vec![Some(9_000), Some(10)], "largest first");
        let mut hours = small_first.clone();
        hours.by = Group::Hour;
        let order: Vec<_> = ranked(&hours)
            .iter()
            .map(|r| r.counters.input_tokens.sum)
            .collect();
        assert_eq!(order, vec![Some(10), Some(9_000)], "hours keep time order");
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
