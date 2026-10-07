//! The pieces chat is drawn from: marks with a presence dot, a tinted
//! row with an accent rail, group headers with a chevron and a guide
//! rail, dividers, a notice line and one-line status. Presentation only;
//! what each one says is decided by the screens that use them.
use super::super::icons::{Icon, icon};
use super::super::style::{Colors, RADIUS_SM, mix, weight};
use super::super::view::{dot, kbd, small};
use crate::app::Message;
use crate::controls::{Kind, custom};
use iced::{
    Center, Color, Element, Fill,
    widget::{Space, column, container, row, stack, text},
};

/// A mark with a presence dot at its lower right: the dot sits in a ring
/// of `ring` (whatever the mark is drawn on) so it reads apart from the
/// tile. `size` is the mark's.
pub(in crate::app) fn with_presence<'a>(
    mark: Element<'a, Message>,
    size: f32,
    tone: Option<Color>,
    ring: Color,
) -> Element<'a, Message> {
    let Some(tone) = tone else {
        return mark;
    };
    let dot_size = if size >= 28.0 { 10.0 } else { 8.0 };
    let outer = size + 2.0;
    let marker =
        container(Space::new().width(dot_size).height(dot_size)).style(move |_| container::Style {
            background: Some(tone.into()),
            border: iced::Border {
                color: ring,
                width: 2.0,
                radius: 999.0.into(),
            },
            ..Default::default()
        });
    stack![
        container(mark).width(outer).height(outer),
        container(marker)
            .width(outer)
            .height(outer)
            .align_right(outer)
            .align_bottom(outer),
    ]
    .into()
}

/// A row with an optional tint behind it and an optional accent rail
/// down its left edge: unread, the open thread's message, the one a
/// notification led to.
pub(in crate::app) fn tinted<'a>(
    content: impl Into<Element<'a, Message>>,
    tint: Option<Color>,
    rail: Option<Color>,
    id: Option<String>,
) -> Element<'a, Message> {
    let mut base = container(content)
        .width(Fill)
        .style(move |_| container::Style {
            background: tint.map(Into::into),
            border: iced::Border {
                radius: RADIUS_SM.into(),
                ..Default::default()
            },
            ..Default::default()
        });
    if let Some(id) = id {
        base = base.id(id);
    }
    match rail {
        None => base.into(),
        Some(rail) => stack![
            base,
            container(Space::new().width(2).height(Fill))
                .height(Fill)
                .padding([3, 0])
                .style(move |_| container::Style {
                    background: Some(rail.into()),
                    border: iced::Border {
                        radius: 1.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
        ]
        .into(),
    }
}

/// A list's section label: short, sentence case, quiet.
pub(in crate::app) fn section_label<'a>(
    label: impl Into<String>,
    c: Colors,
) -> Element<'a, Message> {
    container(
        text(label.into())
            .size(12)
            .font(weight(iced::font::Weight::Medium))
            .color(c.muted),
    )
    .padding(iced::Padding {
        top: 14.0,
        right: 8.0,
        bottom: 4.0,
        left: 8.0,
    })
    .into()
}

/// The header of a group that folds: its label, how many it holds, and a
/// chevron that points where it opens. The spoken label is the whole of
/// it, `From AgentDocker (3)`.
pub(in crate::app) fn group_header<'a>(
    id: &str,
    label: &str,
    count: usize,
    open: bool,
    message: Message,
    c: Colors,
) -> Element<'a, Message> {
    custom(
        id.to_owned(),
        format!("{label} ({count})"),
        row![
            text(label.to_owned())
                .size(12)
                .font(weight(iced::font::Weight::Medium))
                .color(c.muted),
            text(count.to_string()).size(12).color(c.faint),
            Space::new().width(Fill),
            icon(
                if open {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                },
                c.faint,
                12.0,
            ),
        ]
        .spacing(6)
        .align_y(Center),
        Some(message),
        false,
        Kind::Quiet,
        [6, 8],
    )
}

/// An open group's rows, indented behind a one-point guide rail.
pub(in crate::app) fn guided<'a>(
    rows: Vec<Element<'a, Message>>,
    c: Colors,
) -> Element<'a, Message> {
    let mut list = column![].spacing(2).width(Fill);
    for item in rows {
        list = list.push(item);
    }
    row![
        Space::new().width(14),
        container(Space::new().width(1).height(Fill))
            .width(1)
            .height(Fill)
            .style(move |_| c.rule()),
        list,
    ]
    .spacing(6)
    .into()
}

/// A rule with a word in it: the day, centred and quiet.
pub(in crate::app) fn day_divider<'a>(label: String, c: Colors) -> Element<'a, Message> {
    let line = move || container(Space::new().width(Fill).height(1)).style(move |_| c.rule());
    container(
        row![
            line(),
            text(label)
                .size(11)
                .font(weight(iced::font::Weight::Medium))
                .color(c.faint),
            line(),
        ]
        .spacing(12)
        .align_y(Center),
    )
    .padding([10, 8])
    .into()
}

/// Where the unread begins: `New` in the accent, then its line.
pub(in crate::app) fn unread_divider<'a>(c: Colors) -> Element<'a, Message> {
    let tone = mix(c.line, c.accent, 0.55);
    container(
        row![
            text("New")
                .size(11)
                .font(weight(iced::font::Weight::Semibold))
                .color(c.accent),
            container(Space::new().width(Fill).height(1)).style(move |_| c.dot(tone)),
        ]
        .spacing(8)
        .align_y(Center),
    )
    .padding([6, 8])
    .into()
}

/// A rule with a quiet count at its start: `2 replies` under a thread's
/// first message.
pub(in crate::app) fn count_divider<'a>(label: String, c: Colors) -> Element<'a, Message> {
    container(
        row![
            text(label)
                .size(11)
                .font(weight(iced::font::Weight::Medium))
                .color(c.faint),
            container(Space::new().width(Fill).height(1)).style(move |_| c.rule()),
        ]
        .spacing(8)
        .align_y(Center),
    )
    .padding([8, 8])
    .into()
}

/// A small glyph on a round raised disc, centred in a column `column`
/// wide so it lines up with the marks of the messages around it.
pub(in crate::app) fn disc<'a>(glyph: Icon, column: f32, c: Colors) -> Element<'a, Message> {
    container(
        container(icon(glyph, c.faint, 12.0))
            .center(22.0)
            .style(move |_| container::Style {
                background: Some(c.raised.into()),
                border: iced::Border {
                    radius: 999.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
    )
    .center_x(column)
    .into()
}

/// One state line: a dot in its tone, the words, and what can be done
/// about it at the end.
pub(in crate::app) fn status_line<'a>(
    tone: Color,
    words: impl Into<String>,
    ink: Color,
    actions: Vec<Element<'a, Message>>,
    c: Colors,
) -> Element<'a, Message> {
    let mut line = row![
        container(dot(tone, 6.0, c)).center_y(18),
        text(words.into())
            .size(12)
            .line_height(iced::Pixels(18.0))
            .color(ink)
            .width(Fill),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Start);
    for item in actions {
        line = line.push(item);
    }
    line.into()
}

/// A link-sized action in a line of text: muted words that take the
/// accent under the pointer. `spoken` is what assistive technology reads.
pub(in crate::app) fn link<'a>(
    id: impl Into<String>,
    spoken: impl Into<String>,
    words: impl Into<String>,
    message: Option<Message>,
    tone: Color,
) -> Element<'a, Message> {
    custom(
        id,
        spoken,
        text(words.into())
            .size(12)
            .line_height(iced::Pixels(16.0))
            .font(weight(iced::font::Weight::Medium))
            .color(tone),
        message,
        false,
        Kind::Inline,
        [1, 6],
    )
}

/// A link that starts exactly where the words above it start: Show more
/// under a message, lined up with its text.
pub(in crate::app) fn flush_link<'a>(
    id: impl Into<String>,
    words: &str,
    message: Option<Message>,
    tone: Color,
) -> Element<'a, Message> {
    custom(
        id,
        words.to_owned(),
        text(words.to_owned())
            .size(12)
            .line_height(iced::Pixels(16.0))
            .font(weight(iced::font::Weight::Medium))
            .color(tone),
        message,
        false,
        Kind::Inline,
        [2, 0],
    )
}

/// A keycap and what it does: `Enter` send.
pub(in crate::app) fn key_hint<'a>(key: &str, does: &str, c: Colors) -> Element<'a, Message> {
    row![
        kbd(key.to_owned(), c),
        small(does.to_owned(), c).size(11.5).color(c.faint),
    ]
    .spacing(5)
    .align_y(Center)
    .into()
}

/// A slim scrollbar that keeps its own lane, `gap` points beside the
/// list, so rows, cards, counts and the Reply that shows under the
/// pointer are never drawn under it. It is drawn only while the list
/// overflows.
pub(in crate::app) fn slim_scrollbar(gap: f32) -> iced::widget::scrollable::Direction {
    iced::widget::scrollable::Direction::Vertical(
        iced::widget::scrollable::Scrollbar::new()
            .width(6)
            .scroller_width(6)
            .spacing(gap),
    )
}

/// A coarse span for a status line: `<1m`, `3m`, `1h 12m`, `2d`. Refreshed
/// by the window's ordinary sweep, never by a timer of its own.
pub(in crate::app) fn elapsed(seconds: i64) -> String {
    let seconds = seconds.max(0);
    match seconds {
        s if s < 60 => "<1m".to_owned(),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => match (s % 3600) / 60 {
            0 => format!("{}h", s / 3600),
            m => format!("{}h {m}m", s / 3600),
        },
        s => format!("{}d", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::elapsed;

    #[test]
    fn elapsed_spans_are_coarse_and_never_negative() {
        assert_eq!(elapsed(-5), "<1m");
        assert_eq!(elapsed(59), "<1m");
        assert_eq!(elapsed(60), "1m");
        assert_eq!(elapsed(3599), "59m");
        assert_eq!(elapsed(3600), "1h");
        assert_eq!(elapsed(3600 + 12 * 60 + 30), "1h 12m");
        assert_eq!(elapsed(2 * 86_400 + 5), "2d");
    }
}
