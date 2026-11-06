//! The bars above the usage table: the window by hour (a day) or by day
//! (a week, a month), input, cache read, cache write and output stacked
//! in each. Every bucket of the window has its place; one nothing was
//! reported for is a gap, not a zero; the one the clock is in is still
//! filling, said so by a dashed edge and in its tooltip. Series are told
//! apart by their pattern and the legend's words, never by hue alone. The
//! folding is `agentdocker_core::usage::chart`; this draws it.
use super::{counted, counter};
use crate::app::Message;
use crate::app::style::{self, Colors};
use crate::app::view::small;
use agentdocker_core::usage::chart::{Bucket, Step, buckets};
use agentdocker_core::usage::report::{CounterReports, Report};
use agentdocker_core::usage::{CounterReport, compact, thousands};
use chrono::Timelike;
use iced::widget::canvas::{self, Frame, Geometry, LineDash, Path, Stroke};
use iced::{
    Color, Element, Fill, Point, Rectangle, Renderer, Size, Theme, mouse,
    widget::{column, container, row, text},
};
use std::cell::RefCell;

/// How a series is filled, so it is known without its hue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    Solid,
    Diagonal,
    Dots,
    Horizontal,
}

/// Which of a bucket's counters a series stacks.
type Pick = fn(&CounterReports) -> &CounterReport;

/// The four series, bottom to top, each with its pattern. Reasoning is
/// not a fifth: providers count it inside output.
pub const SERIES: [(&str, Pattern, Pick); 4] = [
    ("Input", Pattern::Solid, |c| &c.input_tokens),
    ("Cache read", Pattern::Diagonal, |c| {
        &c.cache_read_input_tokens
    }),
    ("Cache write", Pattern::Dots, |c| {
        &c.cache_write_input_tokens
    }),
    ("Output", Pattern::Horizontal, |c| &c.output_tokens),
];

/// The bars' height.
const HEIGHT: f32 = 96.0;

/// Each series' tint; the pattern is what tells them apart.
fn tints(c: Colors) -> [Color; 4] {
    [c.accent, c.cyan, c.green, c.muted]
}

/// One bar, as drawn: each series' height as a fraction of the chart,
/// `None` where its samples did not say.
#[derive(Clone, Debug, PartialEq)]
struct Bar {
    parts: [Option<f32>; 4],
    tints: [Color; 4],
    /// The pattern's lines: the card's own colour, through the tint.
    ink: Color,
    /// The baseline, and the dashed edge of a bar still filling.
    line: Color,
    filling: bool,
    /// Nothing reported: the baseline alone, a gap.
    empty: bool,
}

/// A bar's geometry, kept between frames while nothing about it changed.
#[derive(Default)]
struct Drawn {
    geometry: canvas::Cache,
    bar: RefCell<Option<Bar>>,
}

impl canvas::Program<Message> for Bar {
    type State = Drawn;

    fn draw(
        &self,
        state: &Drawn,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        if state.bar.borrow().as_ref() != Some(self) {
            state.geometry.clear();
            *state.bar.borrow_mut() = Some(self.clone());
        }
        vec![state.geometry.draw(renderer, bounds.size(), |frame| {
            self.paint(frame, bounds.size());
        })]
    }
}

impl Bar {
    fn paint(&self, frame: &mut Frame, size: Size) {
        let (w, h) = (size.width, size.height);
        frame.fill_rectangle(Point::new(0.0, h - 1.0), Size::new(w, 1.0), self.line);
        if self.empty {
            return;
        }
        let x = 1.0;
        let width = (w - 2.0).max(1.0);
        let room = (h - 2.0).max(0.0);
        let mut y = h - 1.0;
        for (i, part) in self.parts.iter().enumerate() {
            let Some(fraction) = part else {
                continue;
            };
            let height = fraction.clamp(0.0, 1.0) * room;
            if height <= 0.0 {
                continue;
            }
            y -= height;
            let region = Rectangle::new(Point::new(x, y), Size::new(width, height));
            let tint = if self.filling {
                style::alpha(self.tints[i], 0.55)
            } else {
                self.tints[i]
            };
            frame.fill_rectangle(region.position(), region.size(), tint);
            self.pattern(frame, region, SERIES[i].1);
        }
        if self.filling {
            let dashes = [3.0, 2.0];
            frame.stroke(
                &Path::rectangle(Point::new(x, y), Size::new(width, h - 1.0 - y)),
                Stroke {
                    line_dash: LineDash {
                        segments: &dashes,
                        offset: 0,
                    },
                    ..Stroke::default().with_width(1.0).with_color(self.line)
                },
            );
        }
    }

    /// The series' pattern through a segment, clipped to it.
    fn pattern(&self, frame: &mut Frame, region: Rectangle, pattern: Pattern) {
        let ink = self.ink;
        let (w, h) = (region.width, region.height);
        frame.with_clip(region, |frame| match pattern {
            Pattern::Solid => {}
            Pattern::Diagonal => {
                let stroke = Stroke::default().with_width(1.0).with_color(ink);
                let mut offset = -h;
                while offset < w + h {
                    let line = Path::line(Point::new(offset, h), Point::new(offset + h, 0.0));
                    frame.stroke(&line, stroke);
                    offset += 4.0;
                }
            }
            Pattern::Dots => {
                let mut y = 2.0;
                while y < h {
                    let mut x = 2.0;
                    while x < w {
                        frame.fill(&Path::circle(Point::new(x, y), 0.9), ink);
                        x += 4.0;
                    }
                    y += 4.0;
                }
            }
            Pattern::Horizontal => {
                let mut y = 2.0;
                while y < h {
                    frame.fill_rectangle(Point::new(0.0, y), Size::new(w, 1.0), ink);
                    y += 3.0;
                }
            }
        });
    }
}

/// A bucket's span, in the words its tooltip and its accessible name
/// use: hours as `Apr 10 13:00–14:00 UTC`, a whole day as `Apr 10 UTC`,
/// a day the window cuts into by the hours it covers.
pub fn span(bucket: &Bucket, step: Step) -> String {
    let day = bucket.since.format("%b %-d");
    match step {
        Step::Hour => format!(
            "{day} {}–{} UTC",
            bucket.since.format("%H:%M"),
            bucket.until.format("%H:%M")
        ),
        Step::Day => {
            let whole = Step::Day.floor(bucket.since) == bucket.since
                && bucket.until == Step::Day.floor(bucket.until);
            let from = bucket.since.format("%H:%M");
            let until = bucket.until.format("%H:%M");
            if whole {
                format!("{day} UTC")
            } else if bucket.until == Step::Day.floor(bucket.until) {
                format!("{day} from {from} UTC")
            } else if Step::Day.floor(bucket.since) == bucket.since {
                format!("{day} until {until} UTC")
            } else {
                format!("{day} {from}–{until} UTC")
            }
        }
    }
}

/// What a bar says on the pointer and to a reader: its span, that it is
/// still filling, each series' count with its coverage mark, and how
/// many samples — or that nothing was reported.
pub fn detail(bucket: &Bucket, step: Step) -> String {
    let mut lines = vec![span(bucket, step)];
    if bucket.filling {
        lines.push("Still filling".to_owned());
    }
    match &bucket.counters {
        None => lines.push("Nothing reported".to_owned()),
        Some(counters) => {
            for (label, _, pick) in SERIES {
                lines.push(format!("{label} {}", counter(pick(counters))));
            }
            lines.push(format!("{} samples", thousands(bucket.samples)));
        }
    }
    lines.join("\n")
}

/// Whether a bucket's start is written under its bar: every sixth hour
/// on the clock, every day of a week, every fifth day of a month counted
/// back from the last.
fn labelled(index: usize, count: usize, bucket: &Bucket, step: Step) -> bool {
    match step {
        Step::Hour => bucket.since.minute() == 0 && bucket.since.hour().is_multiple_of(6),
        Step::Day if count <= 10 => true,
        Step::Day => (count - 1 - index).is_multiple_of(5),
    }
}

fn label(bucket: &Bucket, step: Step) -> String {
    match step {
        Step::Hour => bucket.since.format("%H:%M").to_string(),
        Step::Day => bucket.since.format("%b %-d").to_string(),
    }
}

/// A swatch of one series, or of a bar still filling, for the legend.
fn swatch<'a>(series: Option<usize>, c: Colors) -> Element<'a, Message> {
    let mut parts = [None; 4];
    parts[series.unwrap_or(0)] = Some(1.0);
    canvas::Canvas::new(Bar {
        parts,
        tints: tints(c),
        ink: c.card,
        line: c.muted,
        filling: series.is_none(),
        empty: false,
    })
    .width(14)
    .height(14)
    .into()
}

/// The chart of a report by hour: its legend, the bars, their labels.
/// Nothing when the window has no buckets; a note when the rows cannot
/// be placed.
pub(in crate::app) fn chart<'a>(hours: &Report, c: Colors) -> Option<Element<'a, Message>> {
    let step = Step::for_range(hours.effective_since, hours.effective_until);
    let buckets = match buckets(hours, step) {
        Ok(buckets) if buckets.is_empty() => return None,
        Ok(buckets) => buckets,
        Err(reason) => {
            return Some(
                crate::app::view::note(format!("Could not draw the chart: {reason}"), c)
                    .color(c.amber)
                    .into(),
            );
        }
    };
    let tallest = buckets.iter().filter_map(Bucket::moved).max().unwrap_or(0);
    let scale = tallest.max(1) as f32;
    let count = buckets.len();
    let mut bars = row![].spacing(2).width(Fill);
    for (index, bucket) in buckets.iter().enumerate() {
        let parts = match &bucket.counters {
            None => [None; 4],
            Some(counters) => {
                SERIES.map(|(_, _, pick)| pick(counters).sum.map(|sum| sum as f32 / scale))
            }
        };
        let bar = canvas::Canvas::new(Bar {
            parts,
            tints: tints(c),
            ink: c.card,
            line: style::alpha(c.text, 0.25),
            filling: bucket.filling,
            empty: bucket.counters.is_none(),
        })
        .width(Fill)
        .height(HEIGHT);
        let words = detail(bucket, step);
        let named = counted(
            format!("usage-bar-{index}"),
            span(bucket, step),
            Some(words),
            bar,
            c,
        );
        let under = if labelled(index, count, bucket, step) {
            label(bucket, step)
        } else {
            String::new()
        };
        bars = bars.push(
            column![
                named,
                text(under)
                    .size(10)
                    .color(c.faint)
                    .wrapping(iced::widget::text::Wrapping::None)
            ]
            .spacing(3)
            .width(Fill),
        );
    }
    let mut legend = row![].spacing(14).align_y(iced::Center);
    for (index, (name, _, _)) in SERIES.iter().enumerate() {
        legend = legend.push(
            row![swatch(Some(index), c), small(*name, c)]
                .spacing(5)
                .align_y(iced::Center),
        );
    }
    if buckets.iter().any(|b| b.filling) {
        legend = legend.push(
            row![swatch(None, c), small("Still filling", c)]
                .spacing(5)
                .align_y(iced::Center),
        );
    }
    let unit = match step {
        Step::Hour => "an hour",
        Step::Day => "a day",
    };
    let header = row![
        legend.wrap(),
        container(small(
            format!("Tallest {} tokens {unit}", compact(tallest)),
            c
        ))
        .width(Fill)
        .align_x(iced::alignment::Horizontal::Right),
    ]
    .spacing(14)
    .align_y(iced::Center);
    Some(
        container(column![header, bars].spacing(10).width(Fill))
            .padding([12, 14])
            .width(Fill)
            .style(move |_| c.card_style())
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::usage::Coverage;
    use chrono::{DateTime, Utc};

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn bucket(since: &str, until: &str) -> Bucket {
        Bucket {
            since: at(since),
            until: at(until),
            samples: 0,
            counters: None,
            filling: false,
        }
    }

    /// A bucket is named by its span, in UTC, as the window cuts it.
    #[test]
    fn a_bucket_is_named_by_its_span() {
        assert_eq!(
            span(
                &bucket("2026-04-10T13:00:00Z", "2026-04-10T14:00:00Z"),
                Step::Hour
            ),
            "Apr 10 13:00–14:00 UTC"
        );
        assert_eq!(
            span(
                &bucket("2026-04-10T00:00:00Z", "2026-04-11T00:00:00Z"),
                Step::Day
            ),
            "Apr 10 UTC"
        );
        assert_eq!(
            span(
                &bucket("2026-04-10T09:00:00Z", "2026-04-11T00:00:00Z"),
                Step::Day
            ),
            "Apr 10 from 09:00 UTC"
        );
        assert_eq!(
            span(
                &bucket("2026-04-10T00:00:00Z", "2026-04-10T16:00:00Z"),
                Step::Day
            ),
            "Apr 10 until 16:00 UTC"
        );
        assert_eq!(
            span(
                &bucket("2026-04-10T09:00:00Z", "2026-04-10T16:00:00Z"),
                Step::Day
            ),
            "Apr 10 09:00–16:00 UTC"
        );
    }

    /// A bar's words: a gap says nothing was reported; a bar says each
    /// series with its mark and its samples; a filling one says so first.
    #[test]
    fn a_bar_says_its_counts_and_coverage_and_a_gap_says_nothing_reported() {
        let gap = bucket("2026-04-10T13:00:00Z", "2026-04-10T14:00:00Z");
        assert_eq!(
            detail(&gap, Step::Hour),
            "Apr 10 13:00–14:00 UTC\nNothing reported"
        );
        let counter = |sum: Option<u64>, coverage| CounterReport {
            sum,
            known_samples: sum.map_or(0, |_| 1),
            coverage,
        };
        let mut filling = bucket("2026-04-10T15:00:00Z", "2026-04-10T16:00:00Z");
        filling.filling = true;
        filling.samples = 3;
        filling.counters = Some(CounterReports {
            input_tokens: counter(Some(12_345), Coverage::Complete),
            cache_read_input_tokens: counter(Some(900), Coverage::Partial),
            cache_write_input_tokens: counter(None, Coverage::Unknown),
            output_tokens: counter(Some(0), Coverage::Complete),
            reasoning_output_tokens: counter(None, Coverage::Unknown),
        });
        assert_eq!(
            detail(&filling, Step::Hour),
            "Apr 10 15:00–16:00 UTC\nStill filling\nInput 12,345\nCache read 900~\nCache write —\nOutput 0\n3 samples"
        );
    }

    /// Labels sit under every sixth hour on the clock, every day of a
    /// week and every fifth day of a month counted from the last.
    #[test]
    fn labels_fall_on_the_clock_and_on_every_fifth_day_from_the_last() {
        let hour = |h: u32| {
            bucket(
                &format!("2026-04-10T{h:02}:00:00Z"),
                &format!("2026-04-10T{:02}:00:00Z", (h + 1) % 24),
            )
        };
        assert!(labelled(0, 24, &hour(6), Step::Hour));
        assert!(!labelled(1, 24, &hour(7), Step::Hour));
        assert_eq!(label(&hour(6), Step::Hour), "06:00");
        let day = bucket("2026-04-10T00:00:00Z", "2026-04-11T00:00:00Z");
        assert!(labelled(3, 7, &day, Step::Day));
        assert!(labelled(29, 30, &day, Step::Day), "the last day");
        assert!(labelled(24, 30, &day, Step::Day));
        assert!(!labelled(28, 30, &day, Step::Day));
        assert_eq!(label(&day, Step::Day), "Apr 10");
    }
}
