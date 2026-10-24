//! Projects, attention and contextual tools over live daemon state.
//!
//! The window is a rail and a workspace. The rail carries the mark, the
//! four destinations and the project list; the workspace leads with the
//! project name, then the section tabs, then whatever the section shows.
//! A state is a labelled chip (`chip::Chip`): an icon and its word, every
//! time, with any fix beside it.
use super::icons::{Icon, icon};
use super::style::{Colors, alpha, weight};
use super::*;
use crate::controls::{
    Kind, button as action, composer, custom, custom_sized, danger, ghost, input, input_submitting,
    primary, segment, tab,
};
use iced::{
    Center, Element, Fill, Font,
    widget::{Space, column, container, row, scrollable, text},
};

/// What a session stopped for the person waits for, as a predicate: what
/// its adapter said, or — read from its quiet screen by the opt-in judge —
/// what it seems to want.
pub(super) fn waiting_errand(
    reason: Option<agentdocker_core::WaitReason>,
    judged: bool,
) -> &'static str {
    use agentdocker_core::WaitReason::{Draft, Permission, Question};
    match (reason, judged) {
        (Some(Permission), false) => "wants permission",
        (Some(Question), false) => "has a question",
        (Some(Draft), false) => "has an unsent draft",
        (_, false) => "is waiting for you",
        (Some(Permission), true) => "seems to want permission",
        (Some(Question), true) => "seems to have a question",
        (Some(Draft), true) => "seems to have an unsent draft",
        (_, true) => "seems to be waiting for you",
    }
}

pub(super) fn heading<'a>(value: impl Into<String>, size: u32) -> iced::widget::Text<'a> {
    text(value.into())
        .size(size)
        .font(weight(iced::font::Weight::Semibold))
}
fn title<'a>(value: impl Into<String>, size: u32) -> iced::widget::Text<'a> {
    text(value.into())
        .size(size)
        .font(weight(iced::font::Weight::Semibold))
}
pub(super) fn note<'a>(value: impl Into<String>, c: Colors) -> iced::widget::Text<'a> {
    text(value.into()).size(13).color(c.muted)
}
/// The typed references beside a card, a message or a hand-off, one per
/// row: the kind as a faint glyph (named on the pointer), the target as
/// text to read, and its note on the target's tooltip. Nothing is opened or
/// copied from here — a path or a pull request is the reader's to open
/// with their own tools.
pub(super) fn links<'a>(links: &[agentdocker_core::Link], c: Colors) -> Element<'a, Message> {
    use agentdocker_core::LinkKind;
    let mut rows = column![].spacing(3).width(Fill);
    for link in links {
        let glyph = match link.kind {
            LinkKind::Path => Icon::File,
            LinkKind::Commit => Icon::Commit,
            LinkKind::Pr => Icon::Code,
            LinkKind::Url => Icon::Connections,
            LinkKind::Task => Icon::Board,
            LinkKind::Message => Icon::Channels,
            LinkKind::Memory => Icon::Note,
        };
        let target = text(link.target.clone())
            .size(12)
            .color(c.accent)
            .wrapping(iced::widget::text::Wrapping::WordOrGlyph);
        let target: Element<'a, Message> = match &link.note {
            Some(note) => tip(target, note.clone(), c),
            None => target.into(),
        };
        rows = rows.push(
            row![
                tip(icon(glyph, c.faint, 12.0), link.kind.as_str().to_owned(), c),
                target
            ]
            .spacing(6)
            .align_y(Center),
        );
    }
    rows.into()
}

pub(super) fn small<'a>(value: impl Into<String>, c: Colors) -> iced::widget::Text<'a> {
    text(value.into()).size(12).color(c.muted)
}
/// A section label: short, quiet, in sentence case. Tracked capitals
/// read as generated; the words stay the size of the text they head.
pub(super) fn eyebrow<'a>(value: impl Into<String>, c: Colors) -> iced::widget::Text<'a> {
    text(value.into())
        .size(12)
        .color(c.faint)
        .font(weight(iced::font::Weight::Medium))
}
fn mono<'a>(value: impl Into<String>, c: Colors) -> iced::widget::Text<'a> {
    text(value.into())
        .size(12)
        .font(Font::MONOSPACE)
        .color(c.muted)
}
/// A card that holds rows rather than prose: tighter padding.
pub(super) fn panel<'a>(
    content: impl Into<Element<'a, Message>>,
    c: Colors,
) -> Element<'a, Message> {
    container(content)
        .padding(4)
        .width(Fill)
        .style(move |_| c.card_style())
        .into()
}
/// A notice in a tone: a faint wash and a hairline of the tone.
fn attention<'a>(
    content: impl Into<Element<'a, Message>>,
    tint: iced::Color,
    c: Colors,
) -> Element<'a, Message> {
    container(content)
        .padding([10, 14])
        .width(Fill)
        .style(move |_| c.attention_style(tint))
        .into()
}
pub(super) fn pill<'a>(
    label: impl Into<String>,
    background: iced::Color,
    ink: iced::Color,
    c: Colors,
) -> Element<'a, Message> {
    container(
        text(label.into())
            .size(11)
            .line_height(iced::Pixels(14.0))
            .font(weight(iced::font::Weight::Medium)),
    )
    .padding([2, 6])
    .style(move |_| c.pill(background, ink))
    .into()
}
pub(super) fn dot<'a>(fill: iced::Color, size: f32, c: Colors) -> Element<'a, Message> {
    container(Space::new().width(size).height(size))
        .style(move |_| c.dot(fill))
        .into()
}
/// A project's mark: its initial on its own tint. The tint comes from
/// the project's identity, so a repository keeps its colour across
/// clones, machines and themes, and two projects side by side are told
/// apart before their names are read.
pub(super) fn monogram<'a>(name: &str, seed: &str, size: f32, c: Colors) -> Element<'a, Message> {
    let (tint, ink) = super::style::identity(seed, c.dark);
    let initial: String = name
        .chars()
        .find(|ch| ch.is_alphanumeric())
        .map(|ch| ch.to_uppercase().collect())
        .unwrap_or_else(|| "·".to_owned());
    container(
        text(initial)
            .size(size * 0.55)
            .font(weight(iced::font::Weight::Semibold))
            .color(ink),
    )
    .center(size)
    .style(move |_| container::Style {
        background: Some(tint.into()),
        border: iced::Border {
            radius: (size * 0.25).round().into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}
/// A tool's own mark on a quiet tile — the card colour and a hairline —
/// so each vendor's colours read the same way in both appearances.
pub(super) fn logo_tile<'a>(
    logo: super::logos::Logo,
    size: f32,
    c: Colors,
) -> Element<'a, Message> {
    let inner = (size * 0.62).round();
    container(
        iced::widget::image(logo.handle(c.dark))
            .width(inner)
            .height(inner),
    )
    .center(size)
    .style(move |_| container::Style {
        background: Some(if c.dark { c.raised } else { c.card }.into()),
        border: iced::Border {
            color: c.line,
            width: 1.0,
            radius: (size * 0.25).round().into(),
        },
        ..Default::default()
    })
    .into()
}
/// An agent's mark: its tool's logo when the tool has one (`runtime` is
/// the registry name, `claude-code`, `codex`, …), otherwise the identity
/// monogram of its name. People and unknown tools keep the monogram.
pub(super) fn agent_mark<'a>(
    runtime: Option<&str>,
    name: &str,
    seed: &str,
    size: f32,
    c: Colors,
) -> Element<'a, Message> {
    match runtime.and_then(super::logos::Logo::for_runtime) {
        Some(logo) => logo_tile(logo, size, c),
        None => monogram(name, seed, size, c),
    }
}
/// A control that says what it does when pointed at: an icon button's
/// name, which its glyph alone does not say; or the detail of a chip,
/// beyond the words it prints. Never a state's meaning: that is the
/// chip's word. Unlike `hint` it adds no padding, since the control is
/// already its own hover target.
pub(super) fn tip<'a>(
    control: impl Into<Element<'a, Message>>,
    label: impl Into<String>,
    c: Colors,
) -> Element<'a, Message> {
    iced::widget::tooltip(
        control,
        container(text(label.into()).size(12).color(c.text))
            .padding([4, 8])
            .style(move |_| container::Style {
                border: iced::Border {
                    radius: super::style::RADIUS_SM.into(),
                    ..c.overlay_style().border
                },
                ..c.overlay_style()
            }),
        iced::widget::tooltip::Position::Bottom,
    )
    .gap(4)
    .into()
}

/// Words a mark stands for, as its accessible name: a reader (and the
/// workflow driver) hears what the eye reads from it. A tooltip alone is
/// not in the accessibility tree.
pub(super) fn said<'a>(
    id: impl Into<String>,
    words: impl Into<String>,
    content: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    crate::controls::Control {
        content: content.into(),
        semantic: crate::accessibility::Semantic {
            role: accesskit::Role::Label,
            ..crate::accessibility::Semantic::button(id.into(), words.into(), None)
        },
        button: false,
    }
    .into()
}

/// A failure in one line: the amber mark and the first line of the
/// words, clipped; all of them on the pointer and in its name.
pub(super) fn error_line<'a>(
    id: impl Into<String>,
    words: &str,
    c: Colors,
) -> Element<'a, Message> {
    let id = id.into();
    // The words are beside the mark; the mark carries all of them, for a
    // reader and on the pointer, where the line is cut.
    let mark = tip(
        said(
            format!("{id}-mark"),
            words.to_owned(),
            container(icon(Icon::Alert, c.amber, 13.0)).padding(3),
        ),
        words.to_owned(),
        c,
    );
    row![
        mark,
        container(
            text(first_line(words, 140))
                .size(13)
                .color(c.amber)
                .wrapping(iced::widget::text::Wrapping::None)
        )
        .width(Fill)
        .clip(true),
    ]
    .spacing(6)
    .align_y(Center)
    .into()
}

/// Something went wrong, as a chip: the alert icon and the head of its
/// words (`chip::headline`), cut to fit a row; all of them its detail when
/// that is not all.
pub(super) fn alert_mark<'a>(
    id: impl Into<String>,
    words: impl Into<String>,
    c: Colors,
) -> Element<'a, Message> {
    let words = words.into();
    let shown = super::chip::headline(&words, 60);
    let mut chip = super::chip::Chip::new(
        id,
        Some(Icon::Alert),
        shown.clone(),
        super::chip::Tone::Attention,
    );
    if shown != words.trim().trim_end_matches('.') {
        chip = chip.detail(words);
    }
    chip.view(c)
}

/// Something small that says what it is when pointed at, with its hover
/// target padded: the rail's project marks (the folder, how many are live
/// or new) and a row's name (its tool and branch).
pub(super) fn hint<'a>(
    content: impl Into<Element<'a, Message>>,
    label: impl Into<String>,
    c: Colors,
) -> Element<'a, Message> {
    // The mark is small; the thing you point at is not. Padding widens
    // the hover target to a comfortable size without moving the mark.
    iced::widget::tooltip(
        container(content).padding(5),
        container(text(label.into()).size(12).color(c.text))
            .padding([4, 8])
            .style(move |_| container::Style {
                border: iced::Border {
                    radius: super::style::RADIUS_SM.into(),
                    ..c.overlay_style().border
                },
                ..c.overlay_style()
            }),
        iced::widget::tooltip::Position::Right,
    )
    .gap(8)
    .into()
}
/// A track holding `segment` choices.
pub(super) fn segmented<'a>(choices: Vec<Element<'a, Message>>, c: Colors) -> Element<'a, Message> {
    let mut track = row![].spacing(2);
    for choice in choices {
        track = track.push(choice);
    }
    container(track)
        .padding(2)
        .style(move |_| container::Style {
            background: Some(if c.dark { c.ground } else { c.raised }.into()),
            border: iced::Border {
                color: c.line,
                width: 1.0,
                radius: super::style::RADIUS_MD.into(),
            },
            ..Default::default()
        })
        .into()
}
/// How much of something is left, as a thin bar.
fn meter<'a>(fraction: f32, fill: iced::Color, c: Colors) -> Element<'a, Message> {
    let left = (fraction.clamp(0.0, 1.0) * 1000.0).round() as u16;
    let mut bar = row![].spacing(0);
    if left > 0 {
        bar = bar.push(
            container(Space::new().width(Fill).height(4))
                .width(iced::Length::FillPortion(left))
                .style(move |_| c.dot(fill)),
        );
    }
    if left < 1000 {
        bar = bar.push(Space::new().width(iced::Length::FillPortion(1000 - left)));
    }
    container(bar)
        .width(Fill)
        .style(move |_| c.dot(alpha(c.text, 0.08)))
        .into()
}
pub(super) fn rule<'a>(c: Colors) -> Element<'a, Message> {
    container(Space::new().width(Fill).height(1))
        .style(move |_| c.rule())
        .into()
}
/// How much of a window from `start` to `end` is still ahead of `now`, as
/// a fraction from 0 (over) to 1 (not begun). A window of no length is over.
fn remaining_fraction(
    start: chrono::DateTime<Utc>,
    end: chrono::DateTime<Utc>,
    now: chrono::DateTime<Utc>,
) -> f32 {
    let total = (end - start).num_milliseconds();
    if total <= 0 {
        return 0.0;
    }
    let left = (end - now).num_milliseconds();
    (left as f64 / total as f64).clamp(0.0, 1.0) as f32
}
/// The first non-empty line of a text, cut to `limit` characters.
pub(super) fn first_line(text: &str, limit: usize) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let mut out: String = line.chars().take(limit).collect();
    if line.chars().count() > limit {
        out.push('…');
    }
    out
}
/// What a message says. Agents and the CLI send `{"text": ...}`, channel
/// notices add a title and room around it; only a payload with no text at
/// all is shown as its JSON.
pub(super) fn spoken_payload(payload: &serde_json::Value) -> String {
    payload
        .as_str()
        .or_else(|| payload["text"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| serde_json::to_string_pretty(payload).unwrap_or_default())
}

/// The mark, decoded once. The PNG is the cube alone on transparency,
/// downscaled for a 30-point slot at two-times density.
fn mark() -> iced::widget::image::Handle {
    static MARK: std::sync::OnceLock<iced::widget::image::Handle> = std::sync::OnceLock::new();
    MARK.get_or_init(|| {
        let mut reader = png::Decoder::new(std::io::Cursor::new(include_bytes!("../mark.png")))
            .read_info()
            .expect("embedded mark");
        let mut rgba = vec![0; reader.output_buffer_size().expect("mark buffer")];
        let info = reader.next_frame(&mut rgba).expect("embedded PNG");
        rgba.truncate(info.buffer_size());
        iced::widget::image::Handle::from_rgba(info.width, info.height, rgba)
    })
    .clone()
}
/// The mark alone on the rail: the window title already names the app.
fn brand<'a>(_r: Colors) -> Element<'a, Message> {
    iced::widget::image(mark()).width(22).height(22).into()
}
/// Nothing here yet: the mark in a quiet tile, or the one thing to do.
/// No words: an empty place should not look like a thing to read.
pub(super) fn empty<'a>(extra: Option<Element<'a, Message>>, c: Colors) -> Element<'a, Message> {
    let body: Element<'a, Message> = match extra {
        Some(extra) => extra,
        None => container(
            iced::widget::image(mark())
                .width(22)
                .height(22)
                .opacity(if c.dark { 0.35_f32 } else { 0.4_f32 }),
        )
        .center(44)
        .style(move |_| c.tile(super::style::RADIUS_LG))
        .into(),
    };
    container(body)
        .padding([36, 20])
        .width(Fill)
        .center_x(Fill)
        .into()
}

/// Status, said the one way: a dot and a word in the same tone.
pub(super) fn status_word<'a>(
    word: impl Into<String>,
    tone: iced::Color,
    c: Colors,
) -> Element<'a, Message> {
    row![
        dot(tone, 7.0, c),
        text(word.into())
            .size(12)
            .font(weight(iced::font::Weight::Medium))
            .color(tone)
    ]
    .spacing(6)
    .align_y(Center)
    .into()
}

/// When a provider's limit lifts, on the person's own clock: soon as a
/// count with the time (*resets in 42 min (15:00)*), later as the day and
/// the time; one already past says so. Round four found every extension
/// studied says it this way rather than in UTC.
pub(super) fn reset_words<Tz: chrono::TimeZone>(
    now: chrono::DateTime<Utc>,
    reset: chrono::DateTime<Utc>,
    zone: &Tz,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let left = reset - now;
    if left <= chrono::Duration::zero() {
        return "should have reset by now".to_owned();
    }
    let local = reset.with_timezone(zone);
    let minutes = (left.num_seconds() + 59) / 60;
    if minutes < 60 {
        format!("resets in {minutes} min ({})", local.format("%H:%M"))
    } else if left < chrono::Duration::hours(24) {
        let (hours, minutes) = (minutes / 60, minutes % 60);
        if minutes == 0 {
            format!("resets in {hours} h ({})", local.format("%H:%M"))
        } else {
            format!(
                "resets in {hours} h {minutes} min ({})",
                local.format("%H:%M")
            )
        }
    } else {
        format!("resets {}", local.format("%b %-d, %H:%M"))
    }
}

/// An agent's standing as its chip, the loudest state only: *Needs you*,
/// *Working*, *Idle*, the same words on every row that shows the agent.
pub(super) fn standing_chip<'a>(
    id: impl Into<String>,
    standing: agentdocker_core::standing::Standing,
    c: Colors,
) -> Element<'a, Message> {
    super::chip::standing(id, standing).view(c)
}

impl App {
    /// The pull request of the branch an agent works on, as one chip that
    /// opens it: its number and its checks in a word — *#12 · Passed*,
    /// *Running*, *Failed*, *Merged*, or *Open* with no checks. Nothing
    /// when the branch has none, or nothing is known.
    pub(super) fn pull_request_chip<'a>(
        &self,
        agent: &AgentRecord,
        c: Colors,
    ) -> Option<Element<'a, Message>> {
        let pull = self.pull_request_of(agent)?;
        Some(custom(
            format!("pull-request-{}", agent.id),
            pull.spoken(),
            pull_request_look(pull).look(c),
            Some(Message::OpenPullRequest(pull.url.clone())),
            false,
            Kind::Inline,
            [0, 0],
        ))
    }
}

/// A pull request as a chip: its number and the word for its checks.
pub(super) fn pull_request_look(pull: &crate::pull_request::PullRequest) -> super::chip::Chip {
    use super::chip::{Chip, Tone};
    use crate::pull_request::Status;
    let (glyph, word, tone) = match pull.status {
        Status::Open => (None, "Open", Tone::Neutral),
        Status::Passed => (Some(Icon::Check), "Passed", Tone::Positive),
        Status::Running => (Some(Icon::Clock), "Running", Tone::Attention),
        Status::Failed => (Some(Icon::Close), "Failed", Tone::Alert),
        Status::Merged => (Some(Icon::LaneDone), "Merged", Tone::Neutral),
    };
    Chip::new(
        format!("pull-request-state-{}", pull.number),
        glyph,
        format!("#{} · {word}", pull.number),
        tone,
    )
}

/// A glyph or a monogram on a small raised tile.
pub(super) fn icon_tile<'a>(
    content: impl Into<Element<'a, Message>>,
    size: f32,
    c: Colors,
) -> Element<'a, Message> {
    container(content)
        .center(size)
        .style(move |_| {
            c.tile(if size >= 32.0 {
                super::style::RADIUS_MD
            } else {
                super::style::RADIUS_SM
            })
        })
        .into()
}

/// A count beside a label: quiet unless it is something waiting on the
/// person, which is amber.
pub(super) fn count_chip<'a>(count: usize, waiting: bool, c: Colors) -> Element<'a, Message> {
    if waiting {
        pill(count.to_string(), alpha(c.amber, 0.16), c.amber, c)
    } else {
        pill(count.to_string(), alpha(c.text, 0.07), c.muted, c)
    }
}

/// One row of a list: a tile, a title over one quiet line, and the row's
/// actions at its right edge. Rows sit in one card separated by rules.
pub(super) fn item_row<'a>(
    media: Option<Element<'a, Message>>,
    title_text: impl Into<String>,
    detail: Option<Element<'a, Message>>,
    trailing: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut words = column![
        text(title_text.into())
            .size(14)
            .font(weight(iced::font::Weight::Medium))
    ]
    .spacing(2)
    .width(Fill);
    if let Some(detail) = detail {
        words = words.push(detail);
    }
    let mut line = row![].spacing(12).align_y(Center);
    if let Some(media) = media {
        line = line.push(media);
    }
    line = line.push(words);
    if let Some(trailing) = trailing {
        line = line.push(trailing);
    }
    container(line).padding([10, 14]).width(Fill).into()
}

/// Rows separated by rules, unframed: the body of a [`section`].
pub(super) fn rows_list<'a>(rows: Vec<Element<'a, Message>>, c: Colors) -> Element<'a, Message> {
    let mut list = column![].width(Fill);
    for (index, item) in rows.into_iter().enumerate() {
        if index > 0 {
            list = list.push(rule(c));
        }
        list = list.push(item);
    }
    list.into()
}

/// Rows in one card, each separated from the next by a rule.
pub(super) fn rows_card<'a>(rows: Vec<Element<'a, Message>>, c: Colors) -> Element<'a, Message> {
    container(rows_list(rows, c))
        .width(Fill)
        .padding(1)
        .style(move |_| c.card_style())
        .into()
}

/// Three faint rows in the shape of the list that will fill this place:
/// a mark, a long bar and a short one, fading downwards. Static.
pub(super) fn ghost_rows<'a>(c: Colors) -> Element<'a, Message> {
    let mut rows = column![].spacing(14).width(Fill);
    for (fade, long) in [(1.0_f32, 168.0_f32), (0.6, 132.0), (0.3, 150.0)] {
        let bar = move |width: f32, height: f32| {
            container(Space::new().width(width).height(height)).style(move |_| container::Style {
                background: Some(alpha(c.text, 0.07 * fade).into()),
                border: iced::Border {
                    radius: super::style::RADIUS_XS.into(),
                    ..Default::default()
                },
                ..Default::default()
            })
        };
        rows = rows.push(
            row![
                bar(28.0, 28.0),
                column![bar(long, 10.0), bar(long * 0.45, 8.0)].spacing(7),
                Space::new().width(Fill),
                bar(56.0, 8.0),
            ]
            .spacing(12)
            .align_y(Center),
        );
    }
    container(rows).padding([28, 24]).width(Fill).into()
}

/// A scrollbar that stays out of the way: no track, a thin scroller in
/// a wash of the ink that firms up under the pointer and while dragged.
pub(super) fn quiet_scroll(
    theme: &iced::Theme,
    status: iced::widget::scrollable::Status,
    c: Colors,
) -> iced::widget::scrollable::Style {
    use iced::widget::scrollable::{self as scroll, Status};
    let mut style = scroll::default(theme, status);
    let strength = match status {
        Status::Dragged { .. } => 0.40,
        Status::Hovered { .. } => 0.28,
        Status::Active { .. } => 0.16,
    };
    for rail in [&mut style.vertical_rail, &mut style.horizontal_rail] {
        rail.background = None;
        rail.border = iced::Border::default();
        rail.scroller.background = alpha(c.text, strength).into();
        rail.scroller.border = iced::Border {
            radius: 3.0.into(),
            ..Default::default()
        };
    }
    style
}

/// A card with a header strip: what it holds on the left, an optional
/// action on the right, a rule, then the body.
pub(super) fn section<'a>(
    title_text: impl Into<String>,
    action_element: Option<Element<'a, Message>>,
    body: impl Into<Element<'a, Message>>,
    c: Colors,
) -> Element<'a, Message> {
    let mut header = row![heading(title_text, 15).width(Fill)]
        .spacing(12)
        .align_y(Center);
    if let Some(action_element) = action_element {
        header = header.push(action_element);
    }
    container(column![
        container(header).padding([12, 16]),
        rule(c),
        container(body).padding([4, 0]).width(Fill),
    ])
    .width(Fill)
    .style(move |_| c.card_style())
    .into()
}

/// Label and value pairs as a definition list in one framed block.
pub(super) fn kv_list<'a>(
    rows: Vec<(String, Element<'a, Message>)>,
    c: Colors,
) -> Element<'a, Message> {
    let mut list = column![].width(Fill);
    for (index, (label, value)) in rows.into_iter().enumerate() {
        if index > 0 {
            list = list.push(rule(c));
        }
        list = list.push(
            container(
                row![small(label, c).width(128), container(value).width(Fill)]
                    .spacing(12)
                    .align_y(Center),
            )
            .padding([7, 12]),
        );
    }
    container(list)
        .width(Fill)
        .style(move |_| container::Style {
            border: iced::Border {
                radius: super::style::RADIUS_MD.into(),
                ..c.card_style().border
            },
            ..c.card_style()
        })
        .into()
}

/// A transient state in one line: a dot, the state word in its tone and
/// the detail after a dash in the quiet ink.
pub(super) fn notice_line<'a>(
    word: impl Into<String>,
    detail: Option<String>,
    tone: iced::Color,
    c: Colors,
) -> Element<'a, Message> {
    let mut line = row![
        dot(tone, 7.0, c),
        text(word.into())
            .size(13)
            .font(weight(iced::font::Weight::Medium))
            .color(tone)
    ]
    .spacing(8)
    .align_y(Center);
    if let Some(detail) = detail {
        line = line.push(text(format!("— {detail}")).size(13).color(c.muted));
    }
    line.into()
}

/// A role as a coloured label: its name in its ink on its tint. A role
/// the palette no longer has is its address, in the quiet ink.
pub(super) fn role_chip<'a>(
    name: String,
    color: Option<agentdocker_core::RoleColor>,
    c: Colors,
) -> Element<'a, Message> {
    let (tint, ink) = color.map_or((c.raised, c.muted), |color| {
        super::style::role_tone(color, c.dark)
    });
    container(
        text(name)
            .size(11.5)
            .color(ink)
            .wrapping(iced::widget::text::Wrapping::None),
    )
    .padding([1, 7])
    .style(move |_| container::Style {
        background: Some(tint.into()),
        border: iced::Border {
            radius: 999.0.into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}

/// A menu's frame: the overlay surface with a little room inside.
pub(super) fn menu<'a>(items: impl Into<Element<'a, Message>>, c: Colors) -> Element<'a, Message> {
    container(items)
        .padding(4)
        .width(Fill)
        .style(move |_| c.overlay_style())
        .into()
}

/// One entry of a menu; a destructive one is said in red.
/// The model a session runs and the effort it was launched with, as its
/// record says them: `gpt-6-astra · xhigh`. `None` when it says neither.
pub(super) fn model_and_effort(agent: &AgentRecord) -> Option<String> {
    let effort = agent
        .spec
        .labels
        .get(agentdocker_core::model_choice::EFFORT_LABEL)
        .filter(|e| agentdocker_core::model_choice::plain_choice(e));
    let model = agent
        .spec
        .model
        .as_deref()
        .filter(|m| agentdocker_core::model_choice::plain_choice(m));
    match (model, effort) {
        (Some(model), Some(effort)) => Some(format!("{model} · {effort}")),
        (Some(model), None) => Some(model.to_owned()),
        (None, Some(effort)) => Some(format!("Effort {effort}")),
        (None, None) => None,
    }
}

pub(super) fn menu_item<'a>(
    id: impl Into<String>,
    label: impl Into<String>,
    message: Option<Message>,
    destructive: bool,
) -> Element<'a, Message> {
    let label = label.into();
    custom(
        id,
        label.clone(),
        text(label).size(13).width(Fill),
        message,
        false,
        if destructive {
            Kind::Destructive
        } else {
            Kind::Quiet
        },
        [5, 8],
    )
}

/// The rule between groups of a menu, inset from its edges.
pub(super) fn menu_separator<'a>(c: Colors) -> Element<'a, Message> {
    container(rule(c)).padding([4, 6]).into()
}

impl App {
    pub fn theme(&self) -> iced::Theme {
        Colors::new(self.shell.catalog.dark).theme()
    }
    pub fn scale_factor(&self) -> f32 {
        self.settings.text_size / 14.0
    }
    /// The runtime of the agent with this id (or a former id of it).
    pub(super) fn runtime_of(&self, id: &str) -> Option<&str> {
        let id = self.canonical_agent(id);
        self.agents
            .iter()
            .find(|a| a.id.as_str() == id)
            .map(|a| a.spec.runtime.as_str())
    }
    /// The mark of the agent with this id: its tool's logo, or its monogram.
    pub(super) fn agent_mark_for<'a>(
        &self,
        id: &str,
        name: &str,
        size: f32,
        c: Colors,
    ) -> Element<'a, Message> {
        agent_mark(self.runtime_of(id), name, id, size, c)
    }
    pub(super) fn selected_root(&self) -> Option<&std::path::Path> {
        self.shell.catalog.selected.as_deref()
    }
    /// Whether a record belongs on the current sessions view: the selected
    /// project's, the projectless ones under Other sessions, or every one
    /// of them on the home view when nothing is selected.
    pub(super) fn has_project(&self, project: Option<&ProjectRef>) -> bool {
        match self.selected_root() {
            Some(root) => project.is_some_and(|p| p.root == root),
            // A session started in `/`, the home folder or a folder that
            // is no absolute path has a project only in name; it is one of
            // the Other sessions.
            None if self.shell.catalog.unassigned => {
                project.is_none_or(|p| self.shell.catalog.projectless(&p.root))
            }
            None => true,
        }
    }
    /// The home view: no project chosen, everything shown.
    pub(super) fn all_projects(&self) -> bool {
        self.selected_root().is_none() && !self.shell.catalog.unassigned
    }
    pub(super) fn narrow(&self) -> bool {
        self.shell.width / self.scale_factor() < 900.0
    }
    pub(super) fn in_project(&self) -> bool {
        !matches!(
            self.screen,
            Screen::Questions
                | Screen::Runtimes
                | Screen::Settings
                | Screen::Desktop
                | Screen::Todo
        )
    }
    /// The strip across the bottom while the person restarts a daemon older
    /// than this window: what it costs before it is done, then that it is
    /// happening. Nothing otherwise.
    fn daemon_notice(&self, c: Colors) -> Option<Element<'_, Message>> {
        self.daemon_behind_now()?;
        let connected = self.connected.is_ok();
        let row = if self.daemon_restarting() {
            row![
                dot(c.amber, 7.0, c),
                small("Restarting the background service…", c).width(Fill),
            ]
        } else if self.shell.confirm_daemon_restart {
            // The question and what it costs, as counts: the sessions this
            // app launched stop, some of them start again; nothing else is
            // said, since nothing else changes.
            let (stop, back) = self.restart_cost();
            let stops = |n: usize| {
                if n == 1 {
                    "1 launched session stops".to_owned()
                } else {
                    format!("{n} launched sessions stop")
                }
            };
            let cost = match (stop, back) {
                (0, _) => String::new(),
                (n, 0) => format!(" {}.", stops(n)),
                (n, m) => format!(
                    " {}, {m} start{} again.",
                    stops(n),
                    if m == 1 { "s" } else { "" }
                ),
            };
            row![
                dot(c.amber, 7.0, c),
                small(format!("Restart the background service?{cost}"), c).width(Fill),
                primary(
                    "daemon-restart-confirm",
                    "Restart",
                    connected.then_some(Message::RestartDaemon),
                ),
                action(
                    "daemon-restart-cancel",
                    "Cancel",
                    Some(Message::ConfirmDaemonRestart(false)),
                    false,
                ),
            ]
        } else {
            // At rest the rail's foot says it (`service_chip`): nothing has
            // to happen now, so it takes no width from the page.
            return None;
        };
        // Confirming or restarting: a strip of the page's own surface,
        // washed amber, carrying the page's controls.
        Some(
            column![
                rule(c),
                container(row.spacing(8).align_y(Center))
                    .padding([5, 14])
                    .width(Fill)
                    .style(move |_| container::Style {
                        background: Some(super::style::mix(c.card, c.amber, 0.08).into()),
                        text_color: Some(c.text),
                        ..Default::default()
                    })
            ]
            .into(),
        )
    }

    /// The words for what an agent is doing, live or finished.
    pub(super) fn activity_label(&self, agent: &AgentRecord) -> String {
        use agentdocker_core::readiness::State;
        let id = agent.id.to_string();
        let readiness = self.readiness_decision(agent).state;
        if self.needs_input(&id) {
            "needs input".to_owned()
        } else if let State::ProviderBlocked(kind) = readiness {
            kind.label().to_owned()
        } else if readiness == State::Paused {
            "not receiving messages".to_owned()
        } else if self.ended_with_undelivered(agent) {
            format!("ended {}", undelivered_phrase(self.undelivered(agent)))
        } else if agent.connector_absent(Utc::now()) || agent.session_absent(Utc::now()) {
            // Registered until revoked or ended, but nothing has come from
            // the browser or the conversation lately: its next request
            // connects it again.
            "not connected".to_owned()
        } else if agent.status.is_live() {
            match self.activity.get(&id) {
                None | Some(Activity::Unknown) => "running".to_owned(),
                Some(Activity::Waiting {
                    reason,
                    judged: Some(judged),
                    ..
                }) => format!("{} · {}", waiting_errand(*reason, true), judged.percent()),
                Some(Activity::Waiting {
                    reason: Some(reason),
                    ..
                }) if *reason != agentdocker_core::WaitReason::Other => {
                    waiting_errand(Some(*reason), false).to_owned()
                }
                Some(Activity::Waiting { .. }) => "waiting for you".to_owned(),
                Some(activity) => activity.label().to_owned(),
            }
        } else if let agentdocker_core::AgentStatus::Failed { reason } = &agent.status {
            format!("failed: {reason}")
        } else {
            "ended".to_owned()
        }
    }

    /// Recent, generation-bound adapter contact; general activity, leases
    /// and saved configuration cannot establish a connection.
    pub(super) fn tool_reports(&self, runtime: &str) -> bool {
        let now = Utc::now();
        self.connected.is_ok()
            && self.agents.iter().any(|a| {
                a.spec.runtime == runtime
                    && a.status.is_live()
                    && (a
                        .adapter_contacts
                        .values()
                        .any(|contact| contact.current_for(a.process_started_at, now))
                        || a.input_delivery.as_ref().is_some_and(|delivery| {
                            delivery.current_for(a.process_started_at, now)
                        }))
            })
    }

    /// What a session's readiness chip says: the one decision, unless the
    /// daemon is not connected, or words wait behind a ready receiver.
    pub(super) fn input_readiness(&self, agent: &AgentRecord) -> super::send_readiness::Readiness {
        use super::send_readiness::Readiness;
        use agentdocker_core::readiness::State;
        let state = self.readiness_decision(agent).state;
        if state == State::Ended {
            return Readiness::Decided(state);
        }
        if self.connected.is_err() {
            return Readiness::Unavailable;
        }
        // A current receiver with words queued that no receipt covers is
        // waiting on the provider to take them; an earlier receipt does not
        // make them delivered. Limits, pauses and silence come first.
        if matches!(state, State::Ready { .. })
            && self
                .awaiting_receipt
                .get(agent.id.as_str())
                .is_some_and(|count| *count > 0)
        {
            return Readiness::Sending;
        }
        Readiness::Decided(state)
    }

    pub fn view(&self) -> Element<'_, Message> {
        let c = Colors::new(self.shell.catalog.dark);
        let narrow = self.narrow();
        // Wide, the rail and the page are two panes with a divider the
        // person drags (the width is kept, in pixels, with the workspace
        // preferences); narrow, the rail is a fixed strip.
        let workspace: Element<'_, Message> = if narrow {
            row![self.sidebar(c), self.page(c)].height(Fill).into()
        } else {
            iced::widget::pane_grid(&self.panes.shell, |_, slot, _| {
                iced::widget::pane_grid::Content::new(match slot {
                    super::panes::Slot::Rail => self.sidebar(c),
                    _ => self.page(c),
                })
            })
            .on_resize(8, |event| {
                Message::PaneResized(super::panes::Grid::Shell, event)
            })
            .style(move |_| split_style(c))
            .height(Fill)
            .into()
        };
        // The bottom strip is there only while a daemon restart is being
        // confirmed or done; the service's state lives at the rail's foot.
        let mut window = column![workspace];
        if let Some(notice) = self.daemon_notice(c) {
            window = window.push(notice);
        }
        let window: Element<'_, Message> = window.into();
        // The window is always the first layer of one stack, dialogs or
        // none, so opening one keeps its scroll offsets and field state.
        let mut layers = vec![window];
        // What just happened floats at the window's foot for a moment and
        // goes, rather than standing under the title as a line to read.
        if !self.status.is_empty() {
            let toast = container(
                row![
                    dot(c.accent, 6.0, c),
                    text(self.status.clone())
                        .size(13)
                        .color(c.text)
                        .wrapping(iced::widget::text::Wrapping::Word)
                ]
                .spacing(8)
                .align_y(Center),
            )
            .padding([8, 14])
            .max_width(520)
            .style(move |_| c.overlay_style());
            layers.push(
                container(toast)
                    .width(Fill)
                    .height(Fill)
                    .align_x(Center)
                    .align_y(iced::alignment::Vertical::Bottom)
                    .padding(20)
                    .into(),
            );
        }
        if self.shell.launch {
            // The launch form is a dialog over the window: a scrim (a flat
            // wash, no blur) that closes it when pressed, and the form
            // centred on it, which takes its own presses.
            let dialog = iced::widget::opaque(
                container(self.launch_view(c))
                    .max_width(520)
                    .style(move |_| c.dialog_style()),
            );
            let scrim = iced::widget::mouse_area(container(dialog).center(Fill).padding(24).style(
                move |_| container::Style {
                    background: Some(
                        alpha(iced::Color::BLACK, if c.dark { 0.6 } else { 0.45 }).into(),
                    ),
                    ..Default::default()
                },
            ))
            .on_press(Message::ShowLaunch);
            layers.push(iced::widget::opaque(scrim));
        }
        // command palette: a layer over everything, the launch dialog
        // included, and its keys ahead of every control (palette.rs).
        if self.shell.palette.open {
            layers.push(self.palette_view(c));
        }
        self.palette_keys(iced::widget::Stack::with_children(layers).into())
    }

    /// The page beside the rail: the header, the tabs and the screen.
    fn page(&self, c: Colors) -> Element<'_, Message> {
        let in_project = self.in_project();
        let narrow = self.narrow();
        let title_text = if in_project {
            self.shell
                .catalog
                .selected()
                .map(|e| e.name())
                .unwrap_or_else(|| {
                    if self.shell.catalog.unassigned {
                        "Other sessions"
                    } else {
                        "All projects"
                    }
                    .into()
                })
        } else {
            match self.screen {
                Screen::Questions if self.has_conversations() => "Messages",
                Screen::Questions => "Inbox",
                Screen::Runtimes => "Tools",
                _ => "Settings",
            }
            .into()
        };
        // The header is one line: the project's mark, its name over its
        // path, then its tools — the hold as words, the terminal as an
        // outline, and the one filled action, Launch agent, at the end.
        let mut heading_row = row![].spacing(12).align_y(Center);
        if in_project && let Some(entry) = self.shell.catalog.selected() {
            heading_row = heading_row.push(monogram(
                &entry.name(),
                &entry.project.id().to_string(),
                32.0,
                c,
            ));
        }
        // The title alone; a project's folder shows when it is pointed at.
        let header_words: Element<'_, Message> =
            match self.shell.catalog.selected().filter(|_| in_project) {
                Some(entry) => container(tip(
                    title(title_text, 20),
                    shorten_home(&entry.project.root),
                    c,
                ))
                .width(Fill)
                .into(),
                None => container(title(title_text, 20)).width(Fill).into(),
            };
        heading_row = heading_row.push(header_words);
        let mut tools = row![].spacing(8).align_y(Center);
        if in_project && let Some(pause) = self.pause_controls(c) {
            tools = tools.push(pause);
        }
        if in_project && self.shell.catalog.selected().is_some() {
            // Launch agent is the header's one labelled action; the rest
            // are icons that say what they do when pointed at.
            tools = tools.push(tip(
                custom(
                    "project-terminal",
                    "Open project terminal",
                    container(icon(Icon::Terminal, c.muted, 15.0)).center(18),
                    (!self.shell.terminal_opening && self.shell.project_available != Some(false))
                        .then_some(Message::OpenProjectTerminal),
                    false,
                    Kind::Ghost,
                    [7, 8],
                ),
                "Open a terminal in the project folder",
                c,
            ));
            if let Some(entry) = self.shell.catalog.selected()
                && let Some(open_in) = self.open_in(
                    super::shell::EditorMenu::Project,
                    &entry.project.root,
                    self.shell.project_available != Some(false),
                    c,
                )
            {
                tools = tools.push(open_in);
            }
        }
        if in_project && let Some(launch) = self.launch_button(c) {
            tools = tools.push(launch);
        }
        let header: Element<'_, Message> = if narrow {
            column![heading_row, tools.wrap()].spacing(12).into()
        } else {
            row![heading_row, tools].spacing(16).align_y(Center).into()
        };
        // Only a project has a header: elsewhere the selected rail row
        // already names the screen.
        let mut content = column![].spacing(16).width(Fill);
        if in_project && self.shell.catalog.selected().is_some() {
            content = content.push(header);
        }
        if in_project && let Some(form) = self.pause_form(c) {
            content = content.push(form);
        }
        // A lost connection is the rail foot's one line, not a banner here.
        if let Some(error) = &self.shell.drafts.error {
            let mut controls = row![].spacing(8);
            if self.shell.drafts.readable {
                controls = controls.push(action(
                    "retry-draft-save",
                    "Retry saving",
                    Some(Message::RetryDraftSave),
                    false,
                ));
            }
            if self.shell.drafts.close_blocked {
                controls = controls.push(action(
                    "close-without-drafts",
                    "Close without saving",
                    Some(Message::CloseWithoutDraftSave),
                    false,
                ));
            }
            content = content.push(attention(
                row![error_line("drafts-error", error, c), controls]
                    .spacing(8)
                    .align_y(Center),
                c.amber,
                c,
            ));
        }
        if let Some(error) = &self.shell.error {
            content = content.push(attention(
                row![
                    error_line("page-error", error, c),
                    ghost("dismiss-error", "Dismiss", Some(Message::DismissError))
                ]
                .spacing(8)
                .align_y(Center),
                c.amber,
                c,
            ));
        }
        if in_project && !self.all_projects() {
            let mut tabs = row![].spacing(18);
            for (screen, label, glyph) in [
                (Screen::Chat, "Chat", Icon::Channels),
                (Screen::Agents, "Agents", Icon::Sessions),
                (Screen::Board, "Board", Icon::Board),
            ] {
                let selected = self.screen == screen
                    || (screen == Screen::Agents && self.screen == Screen::Terminal);
                // The Board tab counts the cards waiting for the person.
                let mark = icon(glyph, if selected { c.text } else { c.muted }, 15.0);
                let waiting = if screen == Screen::Board {
                    self.board_waiting()
                } else {
                    0
                };
                let mark: Element<'_, Message> = if waiting > 0 {
                    row![mark, count_chip(waiting, true, c)]
                        .spacing(5)
                        .align_y(Center)
                        .into()
                } else {
                    mark
                };
                tabs = tabs.push(tab(
                    format!("project-tab-{screen:?}"),
                    label,
                    Some(mark),
                    Some(Message::Navigate(screen)),
                    selected,
                ));
            }
            // Underlined only on one of its own screens: with its menu open
            // over Agents, two tabs read as selected at once.
            let more_selected = matches!(
                self.screen,
                Screen::Journal | Screen::Leases | Screen::Console | Screen::Usage | Screen::Plans
            );
            let more_label = match self.screen {
                Screen::Plans => "Plans",
                Screen::Journal => "History",
                Screen::Leases => "Files in use",
                Screen::Console => "Commands",
                Screen::Usage => "Usage",
                _ => "More",
            };
            let more_tab = tab(
                "project-more",
                more_label,
                Some(icon(
                    Icon::More,
                    if more_selected { c.text } else { c.muted },
                    15.0,
                )),
                Some(Message::More),
                more_selected,
            );
            let more_menu = self.shell.more.then(|| self.more_menu(c));
            tabs = tabs.push(
                crate::controls::popover(more_tab, more_menu)
                    .width(232.0)
                    .on_dismiss(Message::More),
            );
            let tabs: Element<'_, Message> = if narrow {
                scrollable(tabs)
                    .id("project-tabs")
                    .direction(iced::widget::scrollable::Direction::Horizontal(
                        iced::widget::scrollable::Scrollbar::default(),
                    ))
                    .into()
            } else {
                tabs.into()
            };
            content = content.push(column![tabs, rule(c)].spacing(0));
        }
        if self.shell.adding {
            // The form is its folder field and its buttons: Add project
            // opened it and names it.
            content = content.push(panel(
                container(
                    column![
                        row![
                            container(input(
                                "project-path",
                                "Project folder",
                                &self.shell.add_path,
                                Message::AddPath
                            ))
                            .width(Fill),
                            action("browse-folder", "Browse…", Some(Message::PickFolder), false),
                        ]
                        .spacing(8)
                        .align_y(Center),
                        row![
                            Space::new().width(Fill),
                            ghost("cancel-add", "Cancel", Some(Message::ShowAdd)),
                            primary(
                                "pin-folder",
                                "Add project",
                                (!self.shell.add_path.trim().is_empty())
                                    .then_some(Message::ResolveFolder),
                            ),
                        ]
                        .spacing(8)
                        .align_y(Center),
                    ]
                    .spacing(12),
                )
                .padding([12, 16]),
                c,
            ));
        }
        if !self.secrets.reviews.is_empty() {
            content = content.push(self.secret_review_panel(c));
        }
        let body = match self.screen {
            Screen::Chat => self.project_chat_view(c),
            Screen::Agents => self.sessions(c),
            Screen::Board => self.board_view(c),
            Screen::Todo => self.todo_view(c),
            Screen::Plans => self.plans_view(c),
            Screen::Usage => self.usage_view(c),
            Screen::Questions if self.has_conversations() => self.messages_view(c),
            Screen::Questions => self.questions(c),
            Screen::Runtimes => self.connections(c),
            Screen::Terminal => self.terminal_view(c),
            Screen::Journal => self.journal_view(c),
            Screen::Leases => self.coordination(c),
            Screen::Console => self.console_view(c),
            Screen::Settings => self.settings_view(c),
            Screen::Desktop => self.installation_view(c),
        };
        // Chat owns the remaining viewport so its composer never depends on
        // scrolling past the header. Large forms and notices scroll above it.
        // Messages does the same: its columns end at the window's foot.
        let owns_viewport = self.screen == Screen::Chat
            || (self.screen == Screen::Questions && self.has_conversations());
        let workspace: Element<'_, Message> = if owns_viewport {
            column![
                container(scrollable(content).height(iced::Shrink))
                    .max_height(self.shell.height / self.scale_factor() * 0.45),
                container(body).height(Fill),
            ]
            .spacing(18)
            .height(Fill)
            .into()
        } else {
            scrollable(content.push(body))
                .spacing(12)
                .width(Fill)
                .height(Fill)
                .id("workspace-scroll")
                .style(move |theme, status| quiet_scroll(theme, status, c))
                .into()
        };
        // The rail is one step off the page in both appearances, so a
        // hairline parts them.
        let edge = 1.0;
        row![
            container(Space::new().width(edge).height(Fill)).style(move |_| c.rule()),
            container(workspace)
                .padding(if narrow { [18, 18] } else { [24, 30] })
                .width(Fill)
                .style(move |_| c.surface(c.ground, false))
        ]
        .height(Fill)
        .into()
    }

    /// The background service at the foot of the rail, only when there is
    /// something to say, and then one thing with at most one action: not
    /// reachable yet; older than this app, with **Restart…**; or an update
    /// to install. A healthy service says nothing, as the status apps
    /// studied do (Docker Desktop, Tailscale); the page keeps its height.
    fn service_chip(&self, r: Colors) -> Option<Element<'_, Message>> {
        use super::chip::{Chip, Tone};
        // A chip that says the state, and the one action beside it.
        let chip: Element<'_, Message> = if self.connected.is_err() {
            Chip::new(
                "service-reconnecting",
                Some(Icon::Ring),
                "Reconnecting…",
                Tone::Attention,
            )
            .detail("Reconnecting to the background service…")
            .view(r)
        } else if let Some(version) = self.daemon_behind_now() {
            if self.daemon_restarting() || self.shell.confirm_daemon_restart {
                // The strip across the bottom has it now.
                return None;
            }
            row![
                Chip::new(
                    "service-behind",
                    Some(Icon::Alert),
                    "Service is older",
                    Tone::Attention
                )
                .detail(format!(
                    "Background service {version} is older than this app ({}).",
                    env!("CARGO_PKG_VERSION")
                ))
                .view(r),
                ghost(
                    "daemon-restart",
                    "Restart…",
                    Some(Message::ConfirmDaemonRestart(true)),
                ),
            ]
            .spacing(4)
            .align_y(Center)
            .into()
        } else {
            let version = self.desktop.update_available()?;
            crate::controls::custom_sized(
                "open-available-update",
                format!("Update {version} available"),
                row![
                    icon(Icon::Download, r.accent_ink, 12.0),
                    text(format!("Update {version}"))
                        .size(12)
                        .color(r.accent_ink)
                ]
                .spacing(8)
                .align_y(Center),
                Some(Message::Navigate(Screen::Desktop)),
                false,
                Kind::Nav,
                [5, 10],
                Fill,
            )
        };
        Some(container(chip).padding([4, 0]).width(Fill).into())
    }

    /// A destination on the rail: glyph, label and an optional count,
    /// amber when it is something waiting on the person; a count of none
    /// is the *Unread* chip.
    fn nav_item<'a>(
        &self,
        id: &'a str,
        label: &'a str,
        glyph: Icon,
        badge: Option<(usize, bool)>,
        message: Message,
        selected: bool,
    ) -> Element<'a, Message> {
        let r = Colors::new(self.shell.catalog.dark).rail();
        let mut content = row![
            icon(glyph, if selected { r.text } else { r.muted }, 16.0),
            text(label)
                .size(13.5)
                .font(weight(iced::font::Weight::Medium))
                .width(Fill)
        ]
        .spacing(10)
        .align_y(Center);
        let spoken = match &badge {
            Some((0, _)) => format!("{label}, unread"),
            Some((count, _)) => format!("{label} {count}"),
            None => label.to_owned(),
        };
        match badge {
            // Unread with nothing addressed to the person: the word, no
            // number.
            Some((0, _)) => {
                content = content.push(super::chip::chip(
                    format!("{id}-unread"),
                    None,
                    "Unread",
                    super::chip::Tone::Info,
                    r,
                ))
            }
            Some((count, waiting)) => content = content.push(count_chip(count, waiting, r)),
            None => {}
        }
        custom(
            id,
            spoken,
            content,
            Some(message),
            selected,
            Kind::Nav,
            [7, 10],
        )
    }

    /// One project's row in the sidebar, with its menu floating from it
    /// when open.
    fn project_row<'a>(
        &'a self,
        entry: &'a crate::catalog::Entry,
        shared: &std::collections::BTreeSet<String>,
        project_page: bool,
        c: Colors,
    ) -> Element<'a, Message> {
        let r = c.rail();
        let path = entry.project.root.clone();
        let selected = project_page && self.selected_root() == Some(path.as_path());
        let name = entry.name();
        let menu_open = self.shell.project_menu.as_deref() == Some(path.as_path());
        let hovered = self.shell.rail_hover.as_deref() == Some(path.as_path());
        let live = self.live_in(&path);
        let done = self.shell.unviewed_in(&self.agents, &path);
        // The mark's tooltip says where the folder is, and what is live
        // or new there; the row itself is the name.
        let mut hint_text = shorten_home(&path);
        if live > 0 {
            hint_text.push_str(&format!(" · {live} live"));
        }
        if done > 0 {
            hint_text.push_str(&format!(" · {done} new"));
        }
        let seed = entry.project.id().to_string();
        // One line each, clipped rather than wrapped or drawn over the
        // marks beside it. Two folders called the same are told apart
        // by where they are, in one short line.
        let calls = self
            .project_standing(&path)
            .0
            .is_some_and(|s| s.calls_for_attention());
        let label = column![
            text(name.clone())
                .size(13.5)
                .font(weight(if calls {
                    iced::font::Weight::Semibold
                } else {
                    iced::font::Weight::Normal
                }))
                .wrapping(iced::widget::text::Wrapping::None)
        ]
        .spacing(1)
        .width(Fill);
        let label = container(label).width(Fill).clip(true);
        // The mark says the same to a reader as on the pointer: the folder
        // is said nowhere else in the rail or the row's menu.
        let mark = said(
            format!("project-mark-{}", path.display()),
            hint_text.clone(),
            monogram(&name, &seed, 20.0, r),
        );
        let mut content = row![hint(mark, hint_text, c), label]
            .spacing(6)
            .width(Fill)
            .align_y(Center);
        // The loudest agent's chip, which counts the agents that need the
        // person when more than one does: how many run is not a reason to
        // look. The name is cut before the chip is.
        let (loudest, needs) = self.project_standing(&path);
        if let Some(loudest) = loudest {
            let mut chip =
                super::chip::standing(format!("project-standing-{}", path.display()), loudest);
            if needs > 1 {
                chip.words = format!("{} · {needs}", chip.words);
            }
            content = content.push(chip.view(r));
        }
        // The row's own menu button, at the row's right edge, shown only
        // where it is wanted: under the pointer, on the selected row and
        // while its menu is open. Elsewhere it is still there, invisible,
        // for the keyboard and assistive technology.
        let shown = hovered || selected || menu_open;
        let trigger = custom_sized(
            format!("project-menu-{}", path.display()),
            format!("Options for {name}"),
            icon(
                Icon::More,
                if menu_open {
                    r.text
                } else if shown {
                    r.muted
                } else {
                    iced::Color::TRANSPARENT
                },
                12.0,
            ),
            Some(Message::ProjectMenu(path.clone())),
            menu_open,
            Kind::Nav,
            [5, 5],
            iced::Length::Shrink,
        );
        let menu = menu_open.then(|| self.project_menu(entry, c));
        content = content.push(
            crate::controls::popover(trigger, menu)
                .width(220.0)
                .align_end()
                .on_dismiss(Message::ProjectMenu(path.clone())),
        );
        let row_button = custom(
            format!("project-{}", path.display()),
            // Two folders called the same are told apart by where they
            // are, to a reader as on the mark's tooltip.
            if shared.contains(&name) {
                format!(
                    "{}{} · {}",
                    if entry.pinned { "• " } else { "" },
                    name,
                    parent_folder(&path)
                )
            } else {
                format!("{}{}", if entry.pinned { "• " } else { "" }, name)
            },
            content,
            Some(Message::SelectProject(path.clone())),
            selected,
            Kind::Nav,
            [5, 8],
        );
        iced::widget::mouse_area(row_button)
            .on_enter(Message::RailHover(path.clone()))
            .on_exit(Message::RailLeave(path))
            .into()
    }

    /// Whether the Temporary fold is open: the person's choice once made,
    /// otherwise open while a scratch project has a live session or is the
    /// project on view. One rule for the fold as drawn and for what a
    /// click on it negates, so one click always closes an open fold.
    pub(super) fn temporary_fold_open(&self) -> bool {
        self.shell.temporary_open.unwrap_or_else(|| {
            let on_view = self.in_project();
            self.shell
                .catalog
                .projects
                .iter()
                .filter(|e| !e.pinned && crate::catalog::is_scratch(&e.project.root))
                .any(|e| {
                    self.live_in(&e.project.root) > 0
                        || (on_view && self.selected_root() == Some(e.project.root.as_path()))
                })
        })
    }

    /// Live, non-human sessions in the project at `root`, each session once.
    /// A project's agents as the rail shows them: the loudest standing
    /// among them, and how many need the person. A collapsed project takes
    /// its loudest agent's chip, so attention shows without opening it.
    pub(super) fn project_standing(
        &self,
        root: &std::path::Path,
    ) -> (Option<agentdocker_core::standing::Standing>, usize) {
        let standings: Vec<_> = self
            .agents
            .iter()
            .filter(|a| {
                a.spec.runtime != agentdocker_core::HUMAN_RUNTIME
                    && a.project.as_ref().is_some_and(|p| p.root == root)
                    && (self.live_session(a) || self.shell.unviewed_done.contains(a.id.as_str()))
            })
            .map(|a| self.standing(a))
            .collect();
        let needs = standings.iter().filter(|s| s.needs_you()).count();
        (standings.into_iter().min(), needs)
    }

    pub(super) fn live_in(&self, root: &std::path::Path) -> usize {
        self.agents
            .iter()
            .filter(|a| {
                self.live_session(a)
                    && a.spec.runtime != agentdocker_core::HUMAN_RUNTIME
                    && a.project.as_ref().is_some_and(|p| p.root == root)
            })
            .count()
    }

    fn sidebar(&self, c: Colors) -> Element<'_, Message> {
        let r = c.rail();
        let project_page = self.in_project();
        // The mark, and Back and Forward beside it at the window's top
        // left (navigation.rs).
        let top = row![brand(r), self.travel_controls(r, c)]
            .spacing(10)
            .align_y(Center);
        let mut nav = column![container(top).padding([0, 8]), Space::new().height(16)].spacing(2);
        // command palette: its way in, under the brand (palette.rs).
        nav = nav
            .push(self.palette_trigger(r))
            .push(Space::new().height(14));
        let unviewed = self.shell.unviewed_done.len();
        nav = nav.push(self.nav_item(
            "projects",
            "Projects",
            Icon::Projects,
            (unviewed > 0).then_some((unviewed, false)),
            Message::AllProjects,
            project_page,
        ));
        nav = nav.push(self.nav_item(
            "todo",
            "To-do",
            Icon::Todo,
            None,
            Message::Navigate(Screen::Todo),
            self.screen == Screen::Todo,
        ));
        nav = nav.push(self.nav_item(
            "inbox",
            if self.has_conversations() {
                "Messages"
            } else {
                "Inbox"
            },
            Icon::Inbox,
            {
                // A number only for what is addressed to the person (direct
                // messages, open questions), since the count is a reason to
                // look; unread project chat is the word Unread.
                let now = Utc::now();
                if self.has_conversations() {
                    use agentdocker_core::ConversationKind;
                    let direct: u64 = self
                        .conversations
                        .iter()
                        .filter(|c| c.kind == ConversationKind::Dm && self.counts_for_person(c))
                        .map(|c| c.unread)
                        .sum();
                    let chatter = self.unread_total() > direct;
                    if direct > 0 {
                        Some((direct as usize, true))
                    } else if chatter {
                        Some((0, false))
                    } else {
                        None
                    }
                } else {
                    let waiting = self.questions.iter().filter(|q| !q.expired(now)).count()
                        + self.direct_messages().len();
                    (waiting > 0).then_some((waiting, true))
                }
            },
            Message::Navigate(Screen::Questions),
            self.screen == Screen::Questions,
        ));
        nav = nav.push(self.nav_item(
            "connections",
            "Tools",
            Icon::Connections,
            None,
            Message::Navigate(Screen::Runtimes),
            self.screen == Screen::Runtimes,
        ));
        nav = nav.push(Space::new().height(22));
        let mut projects = column![].spacing(1).width(Fill);
        let shared = self.shell.catalog.shared_names();
        // Folders discovered under a scratch directory and never pinned
        // are fixtures and trials, not the person's projects: one group
        // under the real ones, folded while nothing runs in any of them
        // and none is selected, opened by hand otherwise; the live count
        // sits on the fold so nothing running is ever hidden.
        let (temporary, regular): (Vec<&crate::catalog::Entry>, Vec<&crate::catalog::Entry>) = self
            .shell
            .catalog
            .projects
            .iter()
            .partition(|e| !e.pinned && crate::catalog::is_scratch(&e.project.root));
        for entry in regular {
            projects = projects.push(self.project_row(entry, &shared, project_page, c));
        }
        if !temporary.is_empty() {
            let live = temporary
                .iter()
                .map(|e| self.live_in(&e.project.root))
                .sum::<usize>();
            // The person's own choice wins, both ways; until they have
            // made one, the fold opens by itself for a live or selected
            // scratch project — the one rule in `temporary_fold_open`.
            let open = self.temporary_fold_open();
            let label = "Temporary".to_owned();
            let mut fold = row![
                icon(
                    if open {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronRight
                    },
                    r.muted,
                    10.0
                ),
                eyebrow(label.clone(), r).width(Fill),
            ]
            .spacing(8)
            .align_y(Center);
            if live > 0 {
                fold = fold.push(super::chip::chip(
                    "projects-temporary-live",
                    None,
                    format!("{live} live"),
                    super::chip::Tone::Positive,
                    r,
                ));
            }
            projects = projects.push(Space::new().height(6)).push(custom(
                "projects-temporary",
                label,
                fold,
                Some(Message::ToggleTemporary),
                false,
                Kind::Nav,
                [6, 10],
            ));
            if open {
                for entry in temporary {
                    projects = projects.push(self.project_row(entry, &shared, project_page, c));
                }
            }
        }
        if self.agents.iter().any(|a| {
            a.project
                .as_ref()
                .is_none_or(|p| self.shell.catalog.projectless(&p.root))
                && a.spec.runtime != agentdocker_core::HUMAN_RUNTIME
        }) || self.discovered.iter().any(|p| {
            p.project
                .as_ref()
                .is_none_or(|p| self.shell.catalog.projectless(&p.root))
        }) {
            projects = projects.push(custom(
                "unassigned",
                "Other sessions",
                row![
                    icon(Icon::Sessions, r.muted, 14.0),
                    text("Other sessions").size(13.5).width(Fill)
                ]
                .spacing(10)
                .align_y(Center),
                Some(Message::Unassigned),
                self.shell.catalog.unassigned && project_page,
                Kind::Nav,
                [7, 10],
            ));
        }
        nav = nav
            .push(
                // A thin bar: the default one took a name's last letters
                // whenever the list scrolled.
                scrollable(projects)
                    .id("sidebar-projects")
                    .direction(iced::widget::scrollable::Direction::Vertical(
                        iced::widget::scrollable::Scrollbar::new()
                            .width(4)
                            .scroller_width(4)
                            .spacing(2),
                    ))
                    // The rail's own ink for the scroller.
                    .style(move |theme, status| quiet_scroll(theme, status, r))
                    .height(Fill),
            )
            .push(Space::new().height(6))
            .push(container(Space::new().width(Fill).height(1)).style(move |_| r.rule()))
            .push(Space::new().height(6));
        if let Some(chip) = self.service_chip(r) {
            nav = nav.push(container(chip).padding([0, 2]));
        }
        let nav = nav
            .push(self.nav_item(
                "add-project",
                "Add project…",
                Icon::Add,
                None,
                Message::ShowAdd,
                self.shell.adding,
            ))
            .push(self.nav_item(
                "settings",
                "Settings",
                Icon::Settings,
                None,
                Message::Navigate(Screen::Settings),
                matches!(self.screen, Screen::Settings | Screen::Desktop),
            ))
            .push(Space::new().height(4));
        container(nav.height(Fill))
            .padding([18, 10])
            // Wide, the rail is a pane whose divider the person drags;
            // narrow, it keeps a fixed width beside the workspace.
            .width(if self.narrow() {
                iced::Length::Fixed(204.0)
            } else {
                Fill
            })
            .height(Fill)
            .style(move |_| container::Style {
                border: iced::Border::default(),
                ..r.surface(r.ground, false)
            })
            .into()
    }

    /// The menu that floats from a project row: rename, pin, remove.
    /// Removing keeps the folder off the list until it is added again;
    /// nothing on disk changes and no session stops.
    fn project_menu(&self, entry: &crate::catalog::Entry, c: Colors) -> Element<'_, Message> {
        let path = entry.project.root.clone();
        let key = path.display().to_string();
        let mut items = column![].spacing(1).width(Fill);
        if let Some((_, draft)) = self
            .shell
            .project_rename
            .as_ref()
            .filter(|(root, _)| root == &path)
        {
            // The field's placeholder is the folder's own name: what an
            // emptied field shows is what saving it empty goes back to.
            items = items.push(
                container(
                    column![
                        input(
                            format!("project-rename-{key}"),
                            &entry.project.name(),
                            draft,
                            Message::ProjectRenameDraft,
                        ),
                        row![
                            Space::new().width(Fill),
                            primary(
                                format!("project-rename-save-{key}"),
                                "Save",
                                Some(Message::ProjectRenameSubmit),
                            ),
                        ]
                        .spacing(8)
                        .align_y(Center),
                    ]
                    .spacing(8),
                )
                .padding(6),
            );
        } else {
            items = items
                .push(menu_item(
                    format!("project-rename-start-{key}"),
                    "Rename…",
                    Some(Message::ProjectRenameStart(path.clone())),
                    false,
                ))
                .push(menu_item(
                    format!("project-pin-{key}"),
                    if entry.pinned { "Unpin" } else { "Pin" },
                    Some(Message::ProjectPin(path.clone())),
                    false,
                ))
                .push(menu_separator(c))
                .push(menu_item(
                    format!("project-remove-{key}"),
                    "Remove from list",
                    Some(Message::ProjectRemove(path.clone())),
                    true,
                ));
        }
        // The folder is the row mark's tooltip; the menu is its actions.
        menu(items, c)
    }

    /// The roles of an agent's project there are to give, in the palette's
    /// order, once read; `None` before then or from a daemon without a
    /// palette, and then nothing about roles is shown.
    pub(super) fn palette_for(&self, agent: &AgentRecord) -> Option<&[agentdocker_core::RoleDef]> {
        agent
            .project
            .as_ref()
            .and_then(|p| self.palettes.get(&p.root.display().to_string()))
            .and_then(Option::as_deref)
    }

    /// An agent's roles, in its palette's order, each with its definition
    /// when the palette has it.
    fn held_roles<'a>(
        &'a self,
        agent: &'a AgentRecord,
    ) -> Vec<(&'a str, Option<&'a agentdocker_core::RoleDef>)> {
        // Without a palette there is nothing to read a label against.
        let Some(palette) = self.palette_for(agent) else {
            return Vec::new();
        };
        let mut held: Vec<_> = agent
            .roles()
            .into_iter()
            .map(|role| (role, palette.iter().find(|d| d.role == role)))
            .collect();
        held.sort_by_key(|(role, def)| {
            (
                def.and_then(|d| palette.iter().position(|p| p.role == d.role))
                    .unwrap_or(usize::MAX),
                *role,
            )
        });
        held
    }

    /// An agent's roles by name, as people read them.
    pub(super) fn role_names(&self, agent: &AgentRecord) -> Vec<String> {
        self.held_roles(agent)
            .into_iter()
            .map(|(role, def)| def.map_or_else(|| role.to_owned(), |d| d.name.clone()))
            .collect()
    }

    /// An agent's roles as coloured labels: at most `limit` of them, then
    /// how many more. Nothing for an agent without one.
    pub(super) fn role_chips<'a>(
        &'a self,
        agent: &AgentRecord,
        limit: Option<usize>,
        c: Colors,
    ) -> Option<Element<'a, Message>> {
        let held = self.held_roles(agent);
        if held.is_empty() {
            return None;
        }
        let shown = limit.unwrap_or(held.len()).min(held.len());
        let mut chips = row![].spacing(4).align_y(Center);
        for (role, def) in &held[..shown] {
            chips = chips.push(role_chip(
                def.map_or_else(|| (*role).to_owned(), |d| d.name.clone()),
                def.map(|d| d.color),
                c,
            ));
        }
        if held.len() > shown {
            chips = chips.push(role_chip(format!("+{}", held.len() - shown), None, c));
        }
        Some(chips.into())
    }

    /// An agent's roles and the control that gives them: its labels, then
    /// the tag that opens the menu. Nothing for the person, or where the
    /// project has no palette to give from.
    fn roles_control<'a>(&'a self, agent: &AgentRecord, c: Colors) -> Option<Element<'a, Message>> {
        // A palette read and emptied keeps the menu, so roles can be made
        // again; one never read, or a daemon without one, shows nothing.
        if agent.spec.runtime == agentdocker_core::HUMAN_RUNTIME
            || self.palette_for(agent).is_none()
        {
            return None;
        }
        let id = agent.id.to_string();
        let open = self.shell.roles_menu.as_deref() == Some(id.as_str());
        let names = self.role_names(agent);
        let trigger = tip(
            custom_sized(
                "session-roles",
                if names.is_empty() {
                    "Roles".to_owned()
                } else {
                    format!("Roles: {}", names.join(", "))
                },
                icon(Icon::Tag, if open { c.text } else { c.muted }, 12.0),
                Some(Message::RolesMenu(id.clone())),
                open,
                Kind::Ghost,
                [5, 5],
                iced::Length::Shrink,
            ),
            "Roles",
            c,
        );
        let menu = open.then(|| self.roles_menu(agent, c));
        let mut line = row![].spacing(6).align_y(Center);
        if let Some(chips) = self.role_chips(agent, None, c) {
            line = line.push(chips);
        }
        line = line.push(
            // Opens leftward from the tag, inside the panel, as the project
            // menu does from its row's edge.
            crate::controls::popover(trigger, menu)
                .width(240.0)
                .align_end()
                .on_dismiss(Message::RolesMenu(id)),
        );
        Some(line.wrap().into())
    }

    /// The roles menu: each role of the project, ticked where the agent
    /// holds it, pressed to give or take it; below, the palette itself:
    /// each role's next colour and its delete, and a new role by name.
    fn roles_menu<'a>(&'a self, agent: &AgentRecord, c: Colors) -> Element<'a, Message> {
        let id = agent.id.to_string();
        let connected = self.connected.is_ok();
        let palette = self.palette_for(agent).unwrap_or_default();
        let mut items = column![].spacing(1).width(Fill);
        if !self.shell.roles_editing {
            for def in palette {
                let held = agent.holds_role(&def.role);
                let (_, ink) = super::style::role_tone(def.color, c.dark);
                items = items.push(custom(
                    format!("role-pick-{id}-{}", def.role),
                    format!("{}, {}", def.name, if held { "given" } else { "not given" }),
                    row![
                        container(if held {
                            Element::from(icon(Icon::Check, c.text, 11.0))
                        } else {
                            Space::new().into()
                        })
                        .width(14),
                        dot(ink, 8.0, c),
                        text(def.name.clone()).size(13).width(Fill),
                    ]
                    .spacing(8)
                    .align_y(Center),
                    connected.then(|| Message::RoleToggle(id.clone(), def.role.clone())),
                    held,
                    Kind::Quiet,
                    [5, 8],
                ));
            }
            items = items.push(menu_separator(c)).push(menu_item(
                "roles-edit",
                "Edit roles…",
                Some(Message::RolesEdit),
                false,
            ));
        } else {
            for def in palette {
                let (_, ink) = super::style::role_tone(def.color, c.dark);
                let armed =
                    self.shell.role_delete.as_ref().is_some_and(|(role, at)| {
                        *role == def.role && at.elapsed() < CONFIRM_WITHIN
                    });
                let losing = self
                    .agents
                    .iter()
                    .filter(|a| {
                        a.project.as_ref().map(agentdocker_core::ProjectRef::id)
                            == agent.project.as_ref().map(agentdocker_core::ProjectRef::id)
                            && a.holds_role(&def.role)
                    })
                    .count();
                let delete: Element<'a, Message> = if armed {
                    custom(
                        format!("role-delete-{}", def.role),
                        format!(
                            "Delete {}: {losing} agent{} lose{} it",
                            def.name,
                            if losing == 1 { "" } else { "s" },
                            if losing == 1 { "s" } else { "" }
                        ),
                        text("Delete?").size(12),
                        connected.then(|| Message::RoleDelete(def.role.clone())),
                        false,
                        Kind::Destructive,
                        [2, 6],
                    )
                } else {
                    tip(
                        custom_sized(
                            format!("role-delete-{}", def.role),
                            format!("Delete {}", def.name),
                            container(icon(Icon::Close, c.faint, 10.0)).center(14),
                            connected.then(|| Message::RoleDelete(def.role.clone())),
                            false,
                            Kind::Ghost,
                            [2, 2],
                            iced::Length::Shrink,
                        ),
                        "Delete",
                        c,
                    )
                };
                items = items.push(
                    row![
                        tip(
                            custom_sized(
                                format!("role-color-{}", def.role),
                                format!("{}, {}", def.name, def.color.as_str()),
                                container(dot(ink, 9.0, c)).center(14),
                                connected.then(|| Message::RoleRecolor(def.role.clone())),
                                false,
                                Kind::Ghost,
                                [3, 3],
                                iced::Length::Shrink,
                            ),
                            "Next colour",
                            c,
                        ),
                        text(def.name.clone()).size(13).width(Fill),
                        delete,
                    ]
                    .spacing(6)
                    .align_y(Center)
                    .padding([2, 4]),
                );
            }
            let can_add = connected && !self.shell.role_draft.trim().is_empty();
            items = items
                .push(
                    container(input_submitting(
                        "role-new",
                        "New role",
                        &self.shell.role_draft,
                        Message::RoleDraft,
                        connected,
                        can_add.then_some(Message::RoleCreate),
                    ))
                    .padding([4, 4]),
                )
                .push(menu_separator(c))
                .push(menu_item(
                    "roles-done",
                    "Done",
                    Some(Message::RolesEdit),
                    false,
                ));
        }
        menu(items, c)
    }

    /// Whether an agent id belongs to the projects on view.
    fn agent_on_view(&self, id: &str) -> bool {
        self.agents
            .iter()
            .find(|a| a.id.as_str() == id)
            .is_some_and(|a| self.has_project(a.project.as_ref()))
    }

    /// The selected project's root as the daemon's selector, when one is.
    pub(super) fn selected_project_root(&self) -> Option<String> {
        self.shell
            .catalog
            .selected
            .as_ref()
            .map(|root| root.display().to_string())
    }

    /// The pause on the selected project, when it is paused.
    fn selected_pause(&self) -> Option<&agentdocker_core::Pause> {
        let entry = self.shell.catalog.selected()?;
        let id = entry.project.id();
        self.pauses.iter().find(|p| p.project == id)
    }

    /// The project's other places and its management, in the menu that
    /// floats from the More tab: the screen on view is marked.
    fn more_menu(&self, c: Colors) -> Element<'_, Message> {
        let place = |id: &'static str, label: String, screen: Screen| {
            let selected = self.screen == screen;
            custom(
                id,
                label.clone(),
                row![
                    text(label).size(13).width(Fill),
                    if selected {
                        icon(Icon::Check, c.accent_ink, 12.0)
                    } else {
                        Space::new().width(12).height(12).into()
                    }
                ]
                .align_y(Center),
                Some(Message::Navigate(screen)),
                selected,
                Kind::Quiet,
                [5, 8],
            )
        };
        let mut items = column![
            place("project-tab-Plans", "Plans".to_owned(), Screen::Plans),
            place("project-tab-Journal", "History".to_owned(), Screen::Journal),
            place(
                "project-tab-Leases",
                "Files in use".to_owned(),
                Screen::Leases
            ),
            place("project-tab-Usage", "Usage".to_owned(), Screen::Usage),
            place(
                "project-tab-Console",
                "AgentDocker commands".to_owned(),
                Screen::Console
            ),
        ]
        .spacing(1)
        .width(Fill);
        if let Some(entry) = self.shell.catalog.selected() {
            items = items
                .push(menu_separator(c))
                .push(menu_item(
                    "pin-selected",
                    if entry.pinned {
                        "Unpin project"
                    } else {
                        "Pin project"
                    },
                    Some(Message::Unpin),
                    false,
                ))
                .push(menu_item(
                    "forget-project",
                    "Forget project",
                    Some(Message::ForgetProject),
                    true,
                ));
        }
        menu(items, c)
    }

    /// Hold or release the project's agents, in the header: a quiet
    /// Pause… while nothing is held, and while paused the reason as an
    /// amber word with Resume. Typing the reason is [`Self::pause_form`],
    /// a row of its own under the header. The form belongs to the project
    /// it was opened for; on another project the header shows that
    /// project's own state.
    pub(super) fn pause_controls(&self, c: Colors) -> Option<Element<'_, Message>> {
        let root = self.selected_project_root()?;
        let connected = self.connected.is_ok();
        let control = self.pause_states.get(&root);
        let sending = control.is_some_and(|control| control.pending.is_some());
        if let Some(pause) = self.selected_pause() {
            // Paused is a chip that starts the reason; a reason too long
            // for it is whole in the detail.
            let (short, whole) = (
                first_line(&pause.reason, 32),
                first_line(&pause.reason, 120),
            );
            let mut paused = super::chip::Chip::new(
                "project-paused",
                Some(Icon::Pause),
                format!("Paused · {short}"),
                super::chip::Tone::Attention,
            );
            if short != whole {
                paused = paused.detail(whole);
            }
            return Some(
                row![
                    paused.view(c),
                    action(
                        "resume-project",
                        if sending { "Resuming…" } else { "Resume" },
                        (connected && !sending).then_some(Message::ResumeProject(root.clone())),
                        false
                    ),
                ]
                .spacing(6)
                .align_y(Center)
                .into(),
            );
        }
        if control.and_then(|control| control.draft.as_ref()).is_some() {
            return None;
        }
        Some(tip(
            custom(
                "pause-project",
                "Pause…",
                container(icon(Icon::Pause, c.muted, 14.0)).center(18),
                (connected && !sending).then_some(Message::PauseStart(root)),
                false,
                Kind::Ghost,
                [7, 8],
            ),
            "Pause agents",
            c,
        ))
    }

    /// The reason for a pause being typed, under the header: what the
    /// agents will read, Pause agents and Cancel, and the last refusal.
    pub(super) fn pause_form(&self, c: Colors) -> Option<Element<'_, Message>> {
        let root = self.selected_project_root()?;
        let connected = self.connected.is_ok();
        let control = self.pause_states.get(&root);
        let sending = control.is_some_and(|control| control.pending.is_some());
        let error = control.and_then(|control| control.error.as_ref());
        let mut form = column![].spacing(8);
        if self.selected_pause().is_none()
            && let Some(reason) = control.and_then(|control| control.draft.as_ref())
        {
            let ready = connected && !sending && !reason.trim().is_empty();
            let draft_project = root.clone();
            let field = input_submitting(
                "pause-reason",
                "Reason",
                reason,
                move |reason| Message::PauseDraft(draft_project.clone(), reason),
                connected && !sending,
                ready.then_some(Message::PauseSubmit(root.clone())),
            );
            let buttons = row![
                ghost(
                    "pause-cancel",
                    "Cancel",
                    (!sending).then_some(Message::PauseCancel(root.clone())),
                ),
                primary(
                    "pause-submit",
                    if sending {
                        "Pausing…"
                    } else {
                        "Pause agents"
                    },
                    ready.then_some(Message::PauseSubmit(root.clone()))
                ),
            ]
            .spacing(8)
            .align_y(Center);
            form = form.push(if self.narrow() {
                Element::from(column![field, buttons].spacing(8))
            } else {
                row![container(field).width(Fill), buttons]
                    .spacing(10)
                    .align_y(Center)
                    .into()
            });
        } else if error.is_none() {
            return None;
        }
        if let Some(error) = error {
            form = form.push(text(error.clone()).size(13).color(c.amber));
        }
        Some(attention(form, c.amber, c))
    }

    /// The project's one primary action, when there is a project to act
    /// in: Launch agent, with the tools to launch in a menu off its
    /// chevron.
    fn launch_button(&self, c: Colors) -> Option<Element<'_, Message>> {
        self.shell.catalog.selected()?;
        let ready = self.connected.is_ok() && self.shell.project_available != Some(false);
        let main = crate::controls::split_primary(
            "launch-agent",
            "Launch agent…",
            ready.then_some(Message::OpenLaunch),
            crate::controls::Split::Start,
        );
        let chevron = crate::controls::split_primary_glyph(
            "launch-menu",
            "Choose a tool to launch",
            icon(Icon::ChevronDown, c.on_primary, 12.0),
            ready.then_some(Message::LaunchMenu),
        );
        let menu = self.shell.launch_menu.then(|| {
            let mut items = column![].spacing(1).width(Fill);
            for runtime in self.runtimes.iter().filter(|r| r.cli.is_some()) {
                let binary = runtime
                    .cli
                    .as_ref()
                    .map(|p| shorten_home(p))
                    .unwrap_or_default();
                items = items.push(custom(
                    format!("launch-with-{}", runtime.name),
                    format!("Launch {}", runtime.label),
                    row![
                        agent_mark(
                            Some(runtime.name.as_str()),
                            &runtime.label,
                            &runtime.name,
                            24.0,
                            c
                        ),
                        // The tool; where its command lives is its tooltip.
                        container(tip(
                            text(runtime.label.clone())
                                .size(13)
                                .font(weight(iced::font::Weight::Medium)),
                            binary,
                            c
                        ))
                        .width(Fill)
                    ]
                    .spacing(10)
                    .align_y(Center),
                    ready.then(|| Message::LaunchWith(runtime.name.clone())),
                    false,
                    Kind::Quiet,
                    [6, 8],
                ));
            }
            menu(items, c)
        });
        Some(
            row![
                main,
                crate::controls::popover(chevron, menu)
                    .width(260.0)
                    .align_end()
                    .on_dismiss(Message::LaunchMenu)
            ]
            .spacing(1)
            .into(),
        )
    }

    /// The installed editors as **Open in…** offers them: the one chosen
    /// last first.
    pub(super) fn editor_choices(&self) -> Vec<&agentdocker_core::editor::Editor> {
        agentdocker_core::editor::ordered(&self.shell.editors, self.shell.catalog.editor.as_deref())
    }

    /// The folder a session's **Open in…** opens: its working folder,
    /// when that is a worktree of its own rather than the project's.
    pub(super) fn session_editor_folder(agent: &AgentRecord) -> Option<&std::path::Path> {
        agent.project.as_ref()?.worktree.as_ref()?;
        agent.spec.workdir.as_deref()
    }

    /// **Open in…**: `folder` in one of the installed editors, from a
    /// menu of them, or straight away when there is only one (**Open in
    /// Zed**). Nothing when no editor is installed. In the header it is an
    /// outline beside Open terminal; in a session's details, a quiet word
    /// under its folder.
    pub(super) fn open_in(
        &self,
        place: super::shell::EditorMenu,
        folder: &std::path::Path,
        enabled: bool,
        c: Colors,
    ) -> Option<Element<'_, Message>> {
        use super::shell::EditorMenu;
        let editors = self.editor_choices();
        let (label, message) = match editors.as_slice() {
            [] => return None,
            [only] => (
                format!("Open in {}", only.name),
                Message::OpenIn(only.kind.to_owned(), folder.to_owned()),
            ),
            _ => ("Open in…".to_owned(), Message::EditorMenu(place)),
        };
        let trigger = match place {
            // In the header, an icon beside the terminal's, named when
            // pointed at.
            EditorMenu::Project => tip(
                custom_sized(
                    "project-open-in",
                    label.clone(),
                    container(icon(Icon::Code, c.muted, 15.0)).center(18),
                    enabled.then_some(message),
                    false,
                    Kind::Ghost,
                    [7, 8],
                    iced::Length::Shrink,
                ),
                label,
                c,
            ),
            EditorMenu::Session => custom_sized(
                "session-open-in",
                label.clone(),
                row![
                    icon(Icon::Code, c.muted, 12.0),
                    text(label)
                        .size(12)
                        .font(weight(iced::font::Weight::Medium))
                ]
                .spacing(5)
                .align_y(Center),
                enabled.then_some(message),
                false,
                Kind::Ghost,
                [3, 6],
                iced::Length::Shrink,
            ),
        };
        if editors.len() == 1 {
            return Some(trigger);
        }
        let menu =
            (self.shell.editor_menu == Some(place)).then(|| self.open_in_menu(folder, enabled, c));
        Some(
            crate::controls::popover(trigger, menu)
                .width(220.0)
                .align_end()
                .on_dismiss(Message::EditorMenu(place))
                .into(),
        )
    }

    /// The menu under **Open in…**: one row per installed editor, the one
    /// chosen last first.
    pub(super) fn open_in_menu(
        &self,
        folder: &std::path::Path,
        enabled: bool,
        c: Colors,
    ) -> Element<'_, Message> {
        let mut items = column![].spacing(1).width(Fill);
        for editor in self.editor_choices() {
            items = items.push(custom(
                format!("open-in-{}", editor.kind),
                format!("Open in {}", editor.name),
                text(editor.name).size(13).width(Fill),
                enabled.then(|| Message::OpenIn(editor.kind.to_owned(), folder.to_owned())),
                false,
                Kind::Quiet,
                [5, 8],
            ));
        }
        menu(items, c)
    }

    fn sessions(&self, c: Colors) -> Element<'_, Message> {
        let selected = self.shell.selected.as_ref().and_then(|id| {
            self.agents
                .iter()
                .find(|a| a.id.as_str() == self.canonical_agent(id))
        });
        let mut panel_col = column![]
            .spacing(if self.settings.roomy { 20 } else { 14 })
            .width(Fill);
        use super::sessions::Filter;
        let current = self.session_records(Filter::Current);
        let attention_rows = self.session_records(Filter::NeedsInput);
        let earlier = self.session_records(Filter::Earlier);
        let available = self.available_processes();
        let filter = self.shell.session_filter;
        let filters = segmented(
            vec![
                segment(
                    "sessions-current",
                    format!("Current ({})", current.len() + available.len()),
                    Some(Message::SessionFilter(Filter::Current)),
                    filter == Filter::Current,
                ),
                segment(
                    "sessions-attention",
                    format!("Needs input ({})", attention_rows.len()),
                    Some(Message::SessionFilter(Filter::NeedsInput)),
                    filter == Filter::NeedsInput,
                ),
            ],
            c,
        );
        let search = input(
            "session-search",
            "Search",
            &self.shell.search,
            Message::Search,
        );
        if self.narrow() {
            panel_col = panel_col.push(column![search, filters].spacing(10));
        } else if selected.is_some() {
            // The inspector takes 340 px from this column. Keep the filter
            // labels and counts together instead of squeezing them beside search.
            panel_col = panel_col.push(column![filters, search].spacing(10));
        } else {
            panel_col = panel_col.push(
                row![container(filters).width(Fill), container(search).width(260)]
                    .spacing(10)
                    .align_y(Center),
            );
        }
        if let Some(strip) = self.needs_you(c) {
            panel_col = panel_col.push(strip);
        }
        if self.shell.project_available == Some(false) {
            panel_col = panel_col.push(attention(
                row![
                    alert_mark("project-folder-missing", "Project folder unavailable", c),
                    action(
                        "retry-project-folder",
                        "Check again",
                        Some(Message::RetryProject),
                        false
                    )
                ]
                .spacing(8)
                .align_y(Center),
                c.amber,
                c,
            ));
        }
        let records = match filter {
            Filter::Current => current,
            Filter::NeedsInput => attention_rows,
            Filter::Earlier => Vec::new(),
        };
        let count = records.len()
            + if filter == Filter::Current {
                available.len()
            } else {
                0
            };
        if !records.is_empty() {
            panel_col = panel_col.push(self.session_rows(&records, c));
        }
        if filter == Filter::Current && !available.is_empty() {
            let mut rows = Vec::new();
            for process in available {
                let label = super::runtime_label(&process.runtime);
                rows.push(item_row(
                    Some(match super::logos::Logo::for_runtime(&process.runtime) {
                        Some(logo) => logo_tile(logo, 28.0, c),
                        None => icon_tile(
                            text(label.chars().next().unwrap_or('·').to_string())
                                .size(12)
                                .font(weight(iced::font::Weight::Semibold))
                                .color(c.cyan),
                            28.0,
                            c,
                        ),
                    }),
                    label,
                    process.cwd.as_ref().map(|cwd| {
                        text(shorten_home(cwd))
                            .size(12)
                            .font(Font::MONOSPACE)
                            .color(c.faint)
                            .into()
                    }),
                    Some(action(
                        format!("adopt-{}", process.pid),
                        if self.shell.adopting.contains(&process.pid) {
                            "Connecting…"
                        } else {
                            "Connect"
                        },
                        (self.connected.is_ok() && !self.shell.adopting.contains(&process.pid))
                            .then_some(Message::Adopt(process.pid)),
                        false,
                    )),
                ));
            }
            panel_col = panel_col.push(section("Not connected", None, rows_list(rows, c), c));
        }
        // A search can match only the Earlier group, which opens below.
        // Those results must not be paired with a "No matching sessions" card.
        let earlier_matches = filter == Filter::Current
            && !earlier.is_empty()
            && !self.shell.search.trim().is_empty();
        if count == 0 && !earlier_matches {
            if !self.shell.search.is_empty() {
                // A search that found nothing says what it looked for and
                // offers the way back.
                panel_col = panel_col.push(empty(
                    Some(action(
                        "session-search-clear",
                        "Clear search",
                        Some(Message::Search(String::new())),
                        false,
                    )),
                    c,
                ));
            } else {
                // An empty project offers its next step and nothing to read.
                let next = (filter == Filter::Current
                    && !self.all_projects()
                    && self.shell.catalog.selected().is_some())
                .then(|| -> Element<'_, Message> {
                    primary(
                        "empty-launch",
                        "Launch agent…",
                        (self.connected.is_ok() && self.shell.project_available != Some(false))
                            .then_some(Message::OpenLaunch),
                    )
                });
                // An empty list shows the shape of what will fill it.
                panel_col = panel_col.push(column![ghost_rows(c), empty(next, c)].spacing(0));
            }
        }
        if filter == Filter::Current && !earlier.is_empty() {
            // Ended sessions are not a tab: one collapsed group under the
            // current ones, opened by a search that finds something there.
            let open = self.shell.earlier_open || !self.shell.search.trim().is_empty();
            let shown = self.shell.earlier_shown.max(EARLIER_PAGE);
            let mut group = column![crate::controls::custom_sized(
                "sessions-earlier",
                format!("Earlier ({})", earlier.len()),
                row![
                    icon(
                        if open {
                            Icon::ChevronDown
                        } else {
                            Icon::ChevronRight
                        },
                        c.muted,
                        12.0
                    ),
                    text("Earlier")
                        .size(13)
                        .font(weight(iced::font::Weight::Medium))
                        .color(c.muted),
                    text(earlier.len().to_string()).size(12).color(c.faint),
                ]
                .spacing(8)
                .align_y(Center),
                Some(Message::ToggleEarlier),
                false,
                Kind::Ghost,
                [5, 8],
                iced::Length::Shrink,
            )]
            .spacing(6);
            if open {
                // The newest first, a page at a time: an ended session
                // from last week is a click away, not on every screen.
                let page: Vec<&AgentRecord> = earlier.iter().copied().take(shown).collect();
                let older = earlier.len().saturating_sub(page.len());
                group = group.push(self.session_rows(&page, c));
                if older > 0 {
                    group = group.push(ghost(
                        "sessions-earlier-more",
                        format!("Show {} older", older.min(EARLIER_PAGE)),
                        Some(Message::MoreEarlier),
                    ));
                }
            }
            panel_col = panel_col.push(group);
        }
        if let Some(agent) = selected {
            let inspector = self.inspector(agent, c);
            if self.shell.width / self.scale_factor() >= 1120.0 {
                return row![panel_col, container(inspector).width(340)]
                    .spacing(16)
                    .into();
            }
            return inspector;
        }
        panel_col.into()
    }

    /// The rows of one list of sessions in one card, a rule between rows
    /// and a project label before each project's rows when every project
    /// is on view. A row reads left to right: who (the session's mark, its
    /// name, its tool and branch on the pointer), how it is (its standing
    /// chip, then the time left to answer or its pull request), and the
    /// one thing to do with it, only when there is one.
    fn session_rows<'a>(&'a self, records: &[&'a AgentRecord], c: Colors) -> Element<'a, Message> {
        let mut items: Vec<Element<'a, Message>> = Vec::new();
        let mut previous_project = None;
        for (index, agent) in records.iter().enumerate() {
            let project_root = agent.project.as_ref().map(|p| &p.root);
            if self.all_projects() && (index == 0 || previous_project != project_root) {
                items.push(
                    container(eyebrow(
                        agent
                            .project
                            .as_ref()
                            .map(|p| {
                                self.shell
                                    .catalog
                                    .projects
                                    .iter()
                                    .find(|entry| entry.project.root == p.root)
                                    .map(|entry| entry.name())
                                    .unwrap_or_else(|| p.name())
                            })
                            .unwrap_or_else(|| "Other sessions".into()),
                        c,
                    ))
                    .padding([10, 14])
                    .width(Fill)
                    .style(move |_| container::Style {
                        background: Some(alpha(c.text, 0.025).into()),
                        ..Default::default()
                    })
                    .into(),
                );
                previous_project = project_root;
            }
            let id = agent.id.to_string();
            let standing = self.standing(agent);
            let activity = self.activity_label(agent);
            let name = self.display_name(agent);
            // The branch is in the name already when the record has one.
            let branch = agent
                .vcs
                .as_ref()
                .and_then(|v| v.branch.as_deref())
                .filter(|b| !name.contains(b));
            // What its provider reported over the last day, when collection
            // is on and it has any.
            let tokens = self
                .agent_tokens
                .get(&id)
                .map(|t| format!("{} tokens in 24h", t.label()));
            // One line: everything else is a point away, in the name's
            // hint and in the session's details.
            let mut facts = vec![activity.clone(), super::agent_tool_label(agent)];
            if let Some(branch) = branch {
                facts.push(branch.to_owned());
            }
            if let Some(tokens) = tokens {
                facts.push(tokens);
            }
            // Its roles, in the palette's words, said with the rest.
            let roles = self.role_names(agent);
            if !roles.is_empty() {
                facts.push(roles.join(", "));
            }
            let spoken = format!(
                "{name}\n{}{}",
                facts.join(" · "),
                if matches!(
                    standing,
                    agentdocker_core::standing::Standing::FinishedUnseen
                ) {
                    " · finished since you last looked"
                } else {
                    ""
                }
            );
            // Heavier ink when something there is new or waits on the
            // person; muted otherwise, so those rows are what the eye finds.
            let attention = standing.calls_for_attention();
            let name_text = text(name.clone())
                .size(13.5)
                .font(weight(if attention {
                    iced::font::Weight::Semibold
                } else {
                    iced::font::Weight::Normal
                }))
                .color(if attention || agent.status.is_live() {
                    c.text
                } else {
                    c.muted
                })
                .wrapping(iced::widget::text::Wrapping::None);
            let mut named = row![hint(name_text, facts[1..].join(" · "), c)]
                .spacing(6)
                .align_y(Center);
            if let Some(chips) = self.role_chips(agent, Some(2), c) {
                named = named.push(chips);
            }
            // The name is cut before the chips are: the state always shows.
            let mut content = row![
                agent_mark(Some(agent.spec.runtime.as_str()), &name, &id, 20.0, c),
                container(named).width(Fill).clip(true),
                standing_chip(format!("session-standing-{id}"), standing, c),
            ]
            .spacing(8)
            .align_y(Center);
            // One more chip, the first that applies: the time left to
            // answer, else the pull request.
            if standing.needs_you() || standing == agentdocker_core::standing::Standing::Failed {
                if let Some(question) = self.soonest_answer_window(&id) {
                    content = content.push(
                        approvals::time_left_chip(
                            format!("session-time-left-{id}"),
                            question,
                            Utc::now(),
                        )
                        .view(c),
                    );
                }
            } else if let Some(chip) = self.pull_request_chip(agent, c) {
                content = content.push(chip);
            }
            // An ended Claude Code session whose decided fix is its
            // reconnect, and that this window can do, says so on its row:
            // the one thing to do with it is right there, not behind
            // Details. While its resume is on its way the row says so; a
            // blocker (no conversation id or folder, no tool) keeps the row
            // quiet, and Details shows the fix there is.
            if self.reconnects_here(agent) {
                let reconnecting = self.shell.reconnecting.as_deref() == Some(id.as_str());
                content = content.push(custom(
                    format!("row-reconnect-{id}"),
                    format!("Reconnect {name} here"),
                    text(if reconnecting {
                        "Reconnecting…"
                    } else {
                        "Reconnect here"
                    })
                    .size(12.5)
                    .font(weight(iced::font::Weight::Medium)),
                    (!reconnecting && !self.shell.launching && self.connected.is_ok())
                        .then_some(Message::Reconnect(id.clone())),
                    false,
                    Kind::Secondary,
                    [4, 10],
                ));
            }
            items.push(custom(
                format!("session-{id}"),
                spoken,
                content,
                Some(if self.all_projects() {
                    Message::OpenSession(id.clone())
                } else {
                    Message::SelectSession(id.clone())
                }),
                self.shell.selected.as_deref() == Some(id.as_str()),
                Kind::Quiet,
                [if self.settings.roomy { 10 } else { 6 }, 10],
            ));
        }
        rows_card(items, c)
    }

    fn inspector(&self, agent: &AgentRecord, c: Colors) -> Element<'_, Message> {
        let id = agent.id.to_string();
        let stop_armed = self
            .confirm_stop
            .as_ref()
            .is_some_and(|(armed, at)| armed == &id && at.elapsed() < CONFIRM_WITHIN);
        let name = self.display_name(agent);
        let header = row![
            agent_mark(Some(agent.spec.runtime.as_str()), &name, &id, 32.0, c),
            // The name and the row's standing chip, so the panel and the
            // list never disagree; what it is doing, its tool and where it
            // runs are the chip's detail.
            row![
                heading(name.clone(), 15).wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                {
                    let mut words =
                        vec![self.activity_label(agent), super::agent_tool_label(agent)];
                    if let Some(place) = Self::whereabouts(agent).filter(|_| agent.status.is_live())
                    {
                        words.push(place);
                    }
                    super::chip::standing("inspector-standing", self.standing(agent))
                        .detail(super::chip::sentence(&words.join(" · ")))
                        .view(c)
                },
            ]
            .spacing(6)
            .align_y(Center)
            .width(Fill),
            custom_sized(
                "close-session",
                "Back to sessions",
                icon(Icon::Close, c.muted, 12.0),
                Some(Message::CloseSession),
                false,
                Kind::Ghost,
                [7, 7],
                iced::Length::Shrink,
            ),
        ]
        .spacing(12)
        .align_y(Center);
        let mut body = column![].spacing(12);
        let delivery = agent.input_delivery.as_ref();
        let paused = delivery.is_some_and(|d| d.paused_for(agent.process_started_at));
        if let Some(roles) = self.roles_control(agent, c) {
            body = body.push(roles);
        }
        if let Some(model) = model_and_effort(agent) {
            body = body.push(small(model, c));
        }
        if agent.spec.runtime != "human" {
            // How messages reach it: its readiness and what waits for it,
            // each a chip; when it last took one is their detail.
            let last = delivery
                .and_then(|d| d.received_at)
                .map(|at| format!("Last took a message {}", ago(Utc::now(), at)));
            let mut marks = row![].spacing(6).align_y(Center);
            // An ended session's last message is a fact under Details, not
            // a line of its own here.
            if agent.status.is_live() {
                let mut readiness = self.input_readiness(agent).chip("inspector-readiness");
                if let Some(last) = &last {
                    readiness = readiness.detail(last.clone());
                }
                marks = marks.push(readiness.view(c));
            }
            if let Some(count) = self.queued_inputs.get(&id).filter(|n| **n > 0) {
                marks = marks.push(super::chip::chip(
                    "inspector-queued",
                    Some(Icon::Inbox),
                    format!("{count} queued"),
                    super::chip::Tone::Neutral,
                    c,
                ));
            }
            body = body.push(marks);
            if let Some((source, state)) = agentdocker_core::provider_block(agent, &self.agents) {
                let issue = state.issue.as_ref().expect("blocked");
                // The limit is the chip; when it lifts, or when the daemon
                // tries a passing failure again, is printed beside it.
                let when = match issue.reset_at {
                    Some(reset) => Some(reset_words(Utc::now(), reset, &chrono::Local)),
                    // A transient failure the daemon retries by itself.
                    None => state.retry_at().map(|at| {
                        format!(
                            "tries again at {}",
                            at.with_timezone(&chrono::Local).format("%H:%M")
                        )
                    }),
                };
                let mut limit = row![super::chip::chip(
                    "provider-limit",
                    Some(Icon::Hourglass),
                    issue.kind.label(),
                    super::chip::Tone::Attention,
                    c,
                )]
                .spacing(6)
                .align_y(Center);
                if let Some(when) = when {
                    limit = limit.push(small(when, c));
                }
                body = body.push(
                    row![
                        limit,
                        action(
                            "resume-provider",
                            "Resume delivery",
                            self.connected.is_ok().then(|| {
                                Message::ResumeProvider(source.id.to_string(), state.observed_at)
                            }),
                            false,
                        )
                    ]
                    .spacing(6)
                    .align_y(Center),
                );
            }
            // The daemon gave up starting the bound receiver: the one repair a
            // person can make from here, and the only control that makes one.
            if let Some(binding) = agent.input_binding.as_ref().filter(|b| b.restart.exhausted) {
                body = body.push(
                    row![
                        super::chip::Chip::new(
                            "receiver-exhausted",
                            Some(Icon::Alert),
                            "Delivery restart failed",
                            super::chip::Tone::Alert,
                        )
                        .detail(format!(
                            "After {} tries; its messages are kept",
                            binding.restart.attempts
                        ))
                        .view(c),
                        action(
                            "retry-receiver",
                            "Restart delivery",
                            self.connected
                                .is_ok()
                                .then(|| Message::RetryController(agent.id.to_string())),
                            false,
                        )
                    ]
                    .spacing(6)
                    .align_y(Center),
                );
            }
            // An ended session: its messages are kept in its queue. Say how
            // many, how to get them delivered, and let the person put the
            // notice away. Nothing waiting means nothing to review.
            if paused && !agent.status.is_live() && self.ended_with_undelivered(agent) {
                body = body.push(
                    super::chip::Chip::new(
                        "ended-undelivered",
                        Some(Icon::Inbox),
                        match self.undelivered(agent) {
                            Some(count) => format!("{count} not delivered"),
                            None => "Not delivered".to_owned(),
                        },
                        super::chip::Tone::Attention,
                    )
                    .detail(format!(
                        "This session ended {}.",
                        undelivered_phrase(self.undelivered(agent))
                    ))
                    .view(c),
                );
            }
            // Every notice an ended session raises can be put away: it can
            // report no recovery, so a block or a count would otherwise stay.
            if !agent.status.is_live()
                && self.delivery_needs_you(agent)
                && !self.notice_dismissed(agent)
            {
                body = body.push(action(
                    "dismiss-delivery",
                    "Dismiss",
                    Some(Message::DismissDelivery(id.clone())),
                    false,
                ));
            }
            if paused && agent.status.is_live() {
                body = body.push(action(
                    "review-delivery",
                    if self.shell.review_delivery {
                        "Hide review"
                    } else {
                        "Review delivery"
                    },
                    self.connected.is_ok().then_some(Message::ReviewDelivery),
                    false,
                ));
                if self.shell.review_delivery {
                    if let Some(reason) = delivery.and_then(|d| d.pause_reason.as_deref()) {
                        body = body.push(error_line(
                            "delivery-paused",
                            &plain_pause_reason(reason),
                            c,
                        ));
                    }
                    if let Some((log_agent, result)) = &self.session_log
                        && log_agent == &id
                    {
                        match result {
                            Err(error) => {
                                body = body.push(error_line(
                                    "session-log-error",
                                    &format!("Could not read the session log: {error}"),
                                    c,
                                ));
                            }
                            Ok(log) if log.is_empty() => {}
                            Ok(log) => {
                                body =
                                    body.push(scrollable(text(log.as_str()).size(12)).height(120));
                            }
                        }
                    }
                }
            }
        }
        // Answer opens the exact question, the way Needs you does; the
        // Messages screen alone would show no question selected.
        // One filled action: answering, when the session asks something;
        // otherwise its terminal. Everything else steps down.
        let question = self
            .questions
            .iter()
            .find(|q| q.from == id && !q.expired(Utc::now()));
        let mut actions = row![].spacing(8).align_y(Center);
        if let Some(question) = question {
            actions = actions.push(primary(
                "session-reply",
                "Answer",
                Some(Message::OpenQuestion(question.id.clone())),
            ));
        }
        // While a message is being written, sending it is the one thing
        // to do; the terminal steps down to an outline.
        let composing =
            self.shell.session_message && agent.status.is_live() && agent.spec.runtime != "human";
        let terminal_kind = |label: String, id: &'static str, message: Option<Message>| {
            if question.is_some() || composing {
                action(id, label, message, false)
            } else {
                primary(id, label, message)
            }
        };
        if agent.managed && agent.spec.tty && agent.status.is_live() {
            actions = actions.push(terminal_kind(
                "Open terminal".to_owned(),
                "attach-session",
                self.connected
                    .is_ok()
                    .then_some(Message::Attach(id.clone())),
            ));
        } else if Self::terminal_reachable(agent) {
            actions = actions.push(terminal_kind(
                Self::terminal_action(agent),
                "open-original-terminal",
                (!self.shell.terminal_opening).then(|| Message::OpenAgentTerminal(id.clone())),
            ));
        }
        if agent.status.is_live() && agent.spec.runtime != "human" {
            actions = actions.push(action(
                "session-message",
                if self.shell.session_message {
                    "Hide message"
                } else {
                    "Message"
                },
                Some(Message::ComposeSession),
                self.shell.session_message,
            ));
        }
        body = body.push(actions.wrap());
        if composing {
            let draft_key = self.session_draft_key(&id);
            let entry = self.shell.session_drafts.get(&draft_key);
            let draft = entry.map(|entry| &entry.draft);
            let sending = draft.is_some_and(|draft| draft.sending.is_some());
            let value = draft.map_or("", |draft| draft.text.as_str());
            let target_for_notice = draft_key.clone();
            let target = draft_key.clone();
            let send = (!sending && !value.trim().is_empty() && self.connected.is_ok())
                .then_some(Message::SendSession(draft_key));
            body = body
                .push(composer(
                    "session-message-text",
                    target.clone(),
                    "Message",
                    value,
                    move |text| Message::SessionDraft(target.clone(), text),
                    true,
                    send.clone(),
                ))
                .push(if question.is_some() {
                    action(
                        "send-session-message",
                        if sending {
                            "Queueing…"
                        } else {
                            "Send message"
                        },
                        send,
                        false,
                    )
                } else {
                    primary(
                        "send-session-message",
                        if sending {
                            "Queueing…"
                        } else {
                            "Send message"
                        },
                        send,
                    )
                });
            if let Some(notice) = draft.and_then(|draft| {
                super::send_readiness::notice(
                    draft,
                    super::shell::DeliveryTarget::Session(target_for_notice.clone()),
                    c,
                )
            }) {
                body = body.push(notice);
            }
            if let Some(error) = draft.and_then(|draft| draft.error.as_deref()) {
                body = body.push(error_line("session-message-error", error, c));
            } else if entry.is_some_and(|entry| entry.queued.is_some()) {
                let received = entry
                    .and_then(|entry| entry.queued.as_ref())
                    .is_some_and(|id| {
                        delivery
                            .and_then(|d| d.received.as_ref())
                            .is_some_and(|input| input.messages.contains(id))
                    });
                // Waiting in its queue, then taken: a chip that says which.
                body = body.push(if received {
                    super::chip::chip(
                        "session-message-queued",
                        Some(Icon::Check),
                        "Received by agent",
                        super::chip::Tone::Positive,
                        c,
                    )
                } else {
                    super::chip::chip(
                        "session-message-queued",
                        Some(Icon::Clock),
                        "Message saved to queue",
                        super::chip::Tone::Neutral,
                        c,
                    )
                });
            }
        }
        // A name of one's choosing, for a live session: the daemon keeps it
        // unique and every other view follows.
        if agent.status.is_live() && agent.spec.runtime != agentdocker_core::HUMAN_RUNTIME {
            match &self.shell.renaming {
                Some((renaming, draft)) if renaming == &id => {
                    let valid = agentdocker_core::agent::check_name(draft.trim());
                    body = body.push(
                        column![
                            crate::controls::input_submitting(
                                "rename-session",
                                "Name",
                                draft,
                                Message::RenameDraft,
                                true,
                                (self.connected.is_ok() && valid.is_ok())
                                    .then_some(Message::SubmitRename),
                            ),
                            row![
                                action(
                                    "rename-save",
                                    "Save",
                                    (self.connected.is_ok() && valid.is_ok())
                                        .then_some(Message::SubmitRename),
                                    false,
                                ),
                                ghost("rename-cancel", "Cancel", Some(Message::CancelRename)),
                            ]
                            .spacing(6),
                        ]
                        .spacing(6),
                    );
                    if let Err(reason) = valid
                        && !draft.is_empty()
                    {
                        body = body.push(alert_mark("rename-invalid", reason, c));
                    }
                }
                _ => {}
            }
        }
        if let Some(changes) = self.changes_section(agent, c) {
            body = body.push(changes);
        }
        let mut footer = column![].spacing(10);
        let mut footer_line = row![].spacing(4).align_y(Center);
        footer_line = footer_line.push(custom_sized(
            "session-details",
            if self.shell.session_details {
                "Hide details"
            } else {
                "Details"
            },
            row![
                icon(
                    if self.shell.session_details {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronRight
                    },
                    c.muted,
                    12.0
                ),
                text(if self.shell.session_details {
                    "Hide details"
                } else {
                    "Details"
                })
                .size(13)
                .font(weight(iced::font::Weight::Medium))
            ]
            .spacing(6)
            .align_y(Center),
            Some(Message::SessionDetails),
            false,
            Kind::Ghost,
            [5, 8],
            iced::Length::Shrink,
        ));
        if agent.status.is_live()
            && agent.spec.runtime != agentdocker_core::HUMAN_RUNTIME
            && !matches!(&self.shell.renaming, Some((renaming, _)) if renaming == &id)
        {
            footer_line = footer_line.push(ghost(
                "rename-session",
                "Rename…",
                Some(Message::StartRename(id.clone())),
            ));
        }
        // Keep it as something to come back to: its Finished chip
        // returns until it is opened again.
        if agent.spec.runtime != agentdocker_core::HUMAN_RUNTIME
            && !self.shell.unviewed_done.contains(&id)
            && !self.standing(agent).needs_you()
        {
            footer_line = footer_line.push(ghost(
                "mark-unread",
                "Mark unread",
                Some(Message::MarkUnread(id.clone())),
            ));
        }
        footer = footer.push(footer_line);
        if self.shell.session_details {
            // The full id is 32 hex digits with nowhere to wrap: shown short,
            // copied whole.
            let value = |value: String| -> Element<'_, Message> {
                text(value)
                    .size(12.5)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                    .into()
            };
            let mono_value = |value: String| -> Element<'_, Message> {
                text(value)
                    .size(12)
                    .font(Font::MONOSPACE)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                    .into()
            };
            let mut facts: Vec<(String, Element<'_, Message>)> = vec![(
                "Session".to_owned(),
                row![
                    text(agent.id.short().to_string())
                        .size(12)
                        .font(Font::MONOSPACE)
                        .width(Fill),
                    crate::controls::custom_sized(
                        "copy-session-id",
                        "Copy session ID",
                        icon(Icon::Copy, c.muted, 12.0),
                        Some(Message::CopyGuidance(agent.id.to_string())),
                        false,
                        Kind::Ghost,
                        [3, 6],
                        iced::Length::Shrink,
                    ),
                ]
                .align_y(Center)
                .into(),
            )];
            let folder = mono_value(
                agent
                    .spec
                    .workdir
                    .as_ref()
                    .map(|p| shorten_home(p))
                    .unwrap_or_else(|| "Working folder unknown".into()),
            );
            // A session in a worktree of its own is opened there, from under
            // its path, which keeps the column's whole width to wrap in; the
            // project's folder is the header's.
            let open_in = Self::session_editor_folder(agent).and_then(|worktree| {
                self.open_in(super::shell::EditorMenu::Session, worktree, true, c)
            });
            facts.push((
                "Folder".to_owned(),
                match open_in {
                    Some(open_in) => column![folder, open_in].spacing(2).into(),
                    None => folder,
                },
            ));
            facts.push((
                "Last seen".to_owned(),
                value(ago(Utc::now(), agent.last_seen)),
            ));
            // A live session's last message is its readiness chip's detail.
            if !agent.status.is_live()
                && let Some(at) = delivery.and_then(|d| d.received_at)
            {
                facts.push(("Last message".to_owned(), value(ago(Utc::now(), at))));
            }
            if let Some(pid) = agent.pid {
                facts.push(("Process".to_owned(), mono_value(pid.to_string())));
            }
            if let Some(vcs) = &agent.vcs {
                facts.push(("Checkout".to_owned(), value(vcs.describe())));
            }
            if let Some(session) = &agent.session {
                facts.push(("Terminal".to_owned(), value(session.describe())));
            }
            let mut details = column![kv_list(facts, c)].spacing(10);
            // The one fix, done here when the window can: the button that
            // reconnects an ended Claude session in a pane of this window,
            // else the command to paste, never both.
            if let Some(fix) = self.fix_view(agent, "inspector", c) {
                details = details.push(fix);
            }
            footer = footer.push(details);
        }
        // Stop takes two presses: the first arms it in place — the same
        // slot turns solid red and says Confirm stop, with a square to
        // put it back — and the second, within five seconds, sends it.
        if agent.status.is_live() {
            let stop: Element<'_, Message> = if stop_armed {
                column![
                    row![
                        danger(
                            "stop-session",
                            "Confirm stop",
                            self.connected.is_ok().then_some(Message::Stop(id.clone())),
                        ),
                        custom_sized(
                            "stop-session-cancel",
                            "Keep running",
                            icon(Icon::Close, c.muted, 12.0),
                            Some(Message::DisarmStop),
                            false,
                            Kind::Secondary,
                            [9, 9],
                            iced::Length::Shrink,
                        ),
                    ]
                    .spacing(6)
                    .align_y(Center),
                ]
                .spacing(6)
                .into()
            } else {
                action(
                    "stop-session",
                    "Stop session…",
                    self.connected.is_ok().then_some(Message::Stop(id)),
                    false,
                )
            };
            footer = footer.push(stop);
        }
        container(column![
            container(header).padding([14, 16]),
            rule(c),
            container(body).padding([14, 16]),
            rule(c),
            container(footer).padding([12, 16]),
        ])
        .width(Fill)
        .style(move |_| c.card_style())
        .into()
    }

    /// The launch dialog: which tool, a name and its arguments, then
    /// Cancel and Launch at the foot. Each tool is a row with its binary,
    /// the chosen one marked like a selected radio.
    fn launch_view(&self, c: Colors) -> Element<'_, Message> {
        // Start from what the agent should do: the prompt first, the tool
        // as a chip under it, the switches as one row of their own labels,
        // everything else behind More options. No title: the button names
        // the dialog.
        let ready =
            !self.shell.launching && self.shell.launch_runtime.is_some() && self.connected.is_ok();
        let prompt = crate::controls::composer(
            super::shell::LAUNCH_PROMPT,
            "launch",
            "Task",
            &self.shell.launch_prompt,
            Message::LaunchPrompt,
            !self.shell.launching,
            ready.then_some(Message::Launch),
        );
        let mut chips = row![].spacing(6);
        for runtime in self.runtimes.iter().filter(|r| r.cli.is_some()) {
            let chosen = self.shell.launch_runtime.as_deref() == Some(runtime.name.as_str());
            let initial: String = runtime
                .label
                .chars()
                .next()
                .map(|ch| ch.to_uppercase().collect())
                .unwrap_or_default();
            let content = row![
                match super::logos::Logo::for_runtime(&runtime.name) {
                    Some(logo) => logo_tile(logo, 18.0, c),
                    None => icon_tile(
                        text(initial)
                            .size(10)
                            .font(weight(iced::font::Weight::Semibold))
                            .color(c.muted),
                        18.0,
                        c,
                    ),
                },
                text(runtime.label.clone())
                    .size(13)
                    .font(weight(iced::font::Weight::Medium))
                    .color(if chosen { c.text } else { c.muted }),
            ]
            .spacing(7)
            .align_y(Center);
            chips = chips.push(
                container(custom_sized(
                    format!("launch-tool-{}", runtime.name),
                    runtime.label.clone(),
                    content,
                    (!self.shell.launching).then_some(Message::LaunchRuntime(runtime.name.clone())),
                    false,
                    Kind::Inline,
                    [5, 10],
                    iced::Length::Shrink,
                ))
                .style(move |_| container::Style {
                    background: chosen.then(|| c.raised.into()),
                    border: iced::Border {
                        color: if chosen { c.text } else { c.line_strong },
                        width: 1.0,
                        radius: 999.0.into(),
                    },
                    ..Default::default()
                }),
            );
        }
        let mut body = column![prompt, chips.wrap()].spacing(10);
        // The switches say what they are on their own faces; what each
        // means shows when it is pointed at.
        let mut switches = row![].spacing(8).align_y(Center);
        if matches!(
            self.shell.launch_runtime.as_deref(),
            Some("claude-code" | "codex")
        ) {
            // Which model and how hard it thinks: the provider's own default
            // unless chosen, said on the chip.
            if let Some(choices) = self.launch_choices() {
                switches = switches
                    .push(self.launch_choice(super::shell::ChoiceMenu::Model, choices, c))
                    .push(self.launch_choice(super::shell::ChoiceMenu::Effort, choices, c));
            }
            // The normal launch has a receiver. Turning it off is explicit;
            // provider consent remains a separate provider-owned step.
            switches = switches.push(tip(
                action(
                    "launch-idle-input",
                    if self.shell.launch_channel {
                        "Idle messages: On"
                    } else {
                        "Idle messages: Off"
                    },
                    (!self.shell.launching)
                        .then_some(Message::LaunchChannel(!self.shell.launch_channel)),
                    self.shell.launch_channel,
                ),
                if !self.shell.launch_channel {
                    "Messages wait until you interact with this session"
                } else if self.shell.launch_runtime.as_deref() == Some("codex") {
                    "Messages wait until the current turn finishes"
                } else {
                    "Connects Claude's channel; consent in the terminal"
                },
                c,
            ));
        }
        // Its own worktree: the session works on a branch of its own, and
        // its changes are reviewed and merged from its Changes. Files git
        // ignores that the project lists in `.worktreeinclude` are copied
        // in. A folder that is not a git repository has no branches.
        if self.launch_project_is_git() {
            switches = switches.push(tip(
                action(
                    "launch-worktree",
                    if self.shell.launch_worktree {
                        "Own worktree: On"
                    } else {
                        "Own worktree: Off"
                    },
                    (!self.shell.launching)
                        .then_some(Message::LaunchWorktree(!self.shell.launch_worktree)),
                    self.shell.launch_worktree,
                ),
                if self.shell.launch_worktree {
                    "Its own branch and folder; merge from Changes"
                } else {
                    "Works in the project folder, on its branch"
                },
                c,
            ));
        }
        body = body.push(switches.wrap());
        if self.shell.launch_runtime.as_deref() == Some("codex")
            && let Some(root) = &self.shell.launch_codex_untrusted
        {
            body = body.push(tip(
                status_word("Trust unconfirmed", c.amber, c),
                format!(
                    "Open Codex in {} to review trust before launching",
                    shorten_home(root)
                ),
                c,
            ));
        }
        // The name, the arguments and the exact command: there when wanted.
        body = body.push(custom_sized(
            "launch-more",
            if self.shell.launch_more {
                "Fewer options"
            } else {
                "More options"
            },
            row![
                icon(
                    if self.shell.launch_more {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronRight
                    },
                    c.muted,
                    12.0
                ),
                text(if self.shell.launch_more {
                    "Fewer options"
                } else {
                    "More options"
                })
                .size(13)
                .font(weight(iced::font::Weight::Medium))
            ]
            .spacing(6)
            .align_y(Center),
            Some(Message::LaunchMore),
            false,
            Kind::Ghost,
            [4, 6],
            iced::Length::Shrink,
        ));
        if self.shell.launch_more {
            body = body
                .push(input(
                    "launch-name",
                    "Name",
                    &self.shell.launch_name,
                    Message::LaunchName,
                ))
                .push(input(
                    "launch-arguments",
                    "Arguments",
                    &self.shell.launch_arguments,
                    Message::LaunchArguments,
                ));
        }
        let footer = row![
            Space::new().width(Fill),
            ghost(
                "cancel-launch",
                "Cancel",
                (!self.shell.launching).then_some(Message::ShowLaunch),
            ),
            primary(
                "confirm-launch",
                if self.shell.launching {
                    "Launching…"
                } else {
                    "Launch agent"
                },
                ready.then_some(Message::Launch),
            ),
        ]
        .spacing(8)
        .align_y(Center);
        column![
            scrollable(container(body).padding([18, 20])).height(iced::Shrink),
            rule(c),
            container(footer).padding([12, 20]),
        ]
        .into()
    }

    /// The new session's model or effort as a chip that opens the list to
    /// choose from, the provider's default first.
    fn launch_choice<'a>(
        &'a self,
        which: super::shell::ChoiceMenu,
        choices: &'a agentdocker_core::model_choice::ModelChoices,
        c: Colors,
    ) -> Element<'a, Message> {
        let open = self.shell.launch_choice_menu == Some(which);
        let (word, key, chosen, default, options): (_, _, _, _, Vec<(String, String)>) = match which
        {
            super::shell::ChoiceMenu::Model => (
                "Model",
                "model",
                self.shell.launch_model.as_deref(),
                choices.default_model.as_deref(),
                choices
                    .models
                    .iter()
                    .map(|m| (m.id.clone(), m.name.clone()))
                    .collect(),
            ),
            super::shell::ChoiceMenu::Effort => (
                "Effort",
                "effort",
                self.shell.launch_effort.as_deref(),
                choices.default_effort.as_deref(),
                choices
                    .efforts_for(self.launch_model_now().as_deref())
                    .iter()
                    .map(|e| (e.clone(), e.clone()))
                    .collect(),
            ),
        };
        let name_of = |id: &str| {
            options
                .iter()
                .find(|(option, _)| option == id)
                .map_or_else(|| id.to_owned(), |(_, name)| name.clone())
        };
        // An argument that sets it is in force: the chip says so and gives
        // way to it.
        let (given_model, given_effort) = self.launch_given();
        let given = match which {
            super::shell::ChoiceMenu::Model => given_model,
            super::shell::ChoiceMenu::Effort => given_effort,
        };
        let shown = given
            .as_deref()
            .or(chosen)
            .or(default)
            .map_or_else(|| "Default".to_owned(), name_of);
        let open = open && given.is_none();
        let trigger = action(
            format!("launch-{key}"),
            format!("{word}: {shown}"),
            (!self.shell.launching && given.is_none())
                .then_some(Message::LaunchChoiceMenu((!open).then_some(which))),
            chosen.is_some() || given.is_some(),
        );
        let pick = move |value: Option<String>| match which {
            super::shell::ChoiceMenu::Model => Message::LaunchModel(value),
            super::shell::ChoiceMenu::Effort => Message::LaunchEffort(value),
        };
        let popup = open.then(|| {
            let mut items = column![].spacing(1).width(Fill);
            items = items.push(menu_item(
                format!("launch-{key}-default"),
                match default {
                    Some(default) => format!("Default ({})", name_of(default)),
                    None => "Default".to_owned(),
                },
                Some(pick(None)),
                false,
            ));
            for (id, name) in &options {
                items = items.push(menu_item(
                    format!("launch-{key}-{id}"),
                    name.clone(),
                    Some(pick(Some(id.clone()))),
                    false,
                ));
            }
            menu(items, c)
        });
        crate::controls::popover(trigger, popup)
            .width(220.0)
            .on_dismiss(Message::LaunchChoiceMenu(None))
            .into()
    }

    /// Messages sent to the person directly, oldest first, minus the ones
    /// that are questions (those have their own cards).
    fn direct_messages(&self) -> Vec<&agentdocker_core::Envelope> {
        let (_, mut direct) = by_room(&self.inbox);
        direct.retain(|message| !self.questions.iter().any(|q| q.id == message.id));
        direct
    }

    /// This agent's unanswered question with the least of its window
    /// left; `None` when nothing is waiting.
    fn soonest_answer_window(&self, agent: &str) -> Option<&agentdocker_core::Question> {
        let now = Utc::now();
        self.questions
            .iter()
            .filter(|q| q.from == agent && !q.expired(now))
            .min_by(|a, b| {
                remaining_fraction(a.asked_at, a.expires_at, now).total_cmp(&remaining_fraction(
                    b.asked_at,
                    b.expires_at,
                    now,
                ))
            })
    }

    /// The last `keep` messages, plus the one a notification pointed at when
    /// it is older than that, so the anchor it scrolls to exists.
    fn recent_window<'a>(
        &self,
        messages: &[&'a agentdocker_core::Envelope],
        keep: usize,
    ) -> Vec<&'a agentdocker_core::Envelope> {
        let start = messages.len().saturating_sub(keep);
        let mut window: Vec<_> = messages[start..].to_vec();
        if let Some(wanted) = &self.shell.notification_message
            && let Some(older) = messages[..start].iter().find(|m| m.id == *wanted)
        {
            window.insert(0, older);
        }
        window
    }

    /// History: one row per journal entry, newest first, in one card —
    /// what kind of thing happened on a tile, the line itself, what kind
    /// and where under it, and how long ago at the right.
    fn journal_view(&self, c: Colors) -> Element<'_, Message> {
        if self.journal.is_empty() {
            return empty(None, c);
        }
        let mut rows = column![].width(Fill);
        for (index, entry) in self.journal.iter().rev().enumerate() {
            if index > 0 {
                rows = rows.push(rule(c));
            }
            rows = rows.push(self.history_row(entry, c));
        }
        container(rows)
            .width(Fill)
            .style(move |_| c.card_style())
            .into()
    }

    /// One journal entry as a row. The kind is said under the line, with
    /// the branch, the commit and the worktree it happened in where the
    /// entry has them; the branch leaves the line so it is said once.
    fn history_row(
        &self,
        entry: &agentdocker_core::JournalEntry,
        c: Colors,
    ) -> Element<'_, Message> {
        use agentdocker_core::JournalKind;
        let (glyph, kind) = match entry.kind {
            JournalKind::Commit => (Icon::Commit, "Commit"),
            JournalKind::Release => (Icon::Check, "Finished work"),
            JournalKind::Note => (Icon::Note, "Note"),
            JournalKind::Join => (Icon::Join, "Joined"),
            JournalKind::Leave => (Icon::Leave, "Left"),
            JournalKind::Handoff => (Icon::Handoff, "Hand-off"),
            JournalKind::Review => (Icon::Hash, "Channel"),
        };
        let mut line = self.journal_line(entry);
        let mut detail = row![small(kind, c)].spacing(0).align_y(Center);
        let mut part = |value: String, code: bool| {
            let shown = if code {
                text(value).size(11).font(Font::MONOSPACE).color(c.muted)
            } else {
                small(value, c)
            };
            detail = std::mem::replace(&mut detail, row![])
                .push(small(" · ", c).color(c.faint))
                .push(shown);
        };
        if let Some(branch) = &entry.branch {
            line = line.replacen(&format!(" [{branch}]"), "", 1);
            // An arrival says its branch in its own words already.
            if !line.contains(branch.as_str()) {
                part(branch.clone(), true);
            }
        }
        if entry.kind == JournalKind::Commit
            && let Some(head) = &entry.head_after
        {
            part(head.chars().take(7).collect(), true);
        }
        if let Some(worktree) = &entry.worktree {
            part(
                format!(
                    "worktree {}",
                    worktree.file_name().map_or_else(
                        || shorten_home(worktree),
                        |n| n.to_string_lossy().into_owned()
                    )
                ),
                false,
            );
        }
        container(
            row![
                icon_tile(icon(glyph, c.muted, 14.0), 28.0, c),
                column![
                    text(line)
                        .size(13)
                        .font(weight(iced::font::Weight::Medium))
                        .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                    detail.wrap(),
                ]
                .spacing(3)
                .width(Fill),
                text(ago(Utc::now(), entry.at)).size(11).color(c.faint),
            ]
            .spacing(12)
            .align_y(Center),
        )
        .padding([10, 16])
        .width(Fill)
        .into()
    }

    /// Files in use: every lease in the project as a row in one card —
    /// what is held, in monospace, over who holds it and why; a short
    /// meter of the time left that turns amber under one fifth; and the
    /// time itself.
    fn coordination(&self, c: Colors) -> Element<'_, Message> {
        let now = Utc::now();
        let mut rows = column![].width(Fill);
        let mut count = 0;
        for lease in &self.leases {
            let holder = self.agents.iter().find(|a| a.id == lease.holder);
            if !holder.is_some_and(|a| self.has_project(a.project.as_ref())) {
                continue;
            }
            if count > 0 {
                rows = rows.push(rule(c));
            }
            count += 1;
            let left_secs = (lease.expires_at - now).num_seconds();
            let left = remaining_fraction(lease.acquired_at, lease.expires_at, now);
            let low = left < 0.2;
            let mut about = vec![
                self.name_of(lease.holder.as_str()),
                match lease.mode {
                    agentdocker_core::LeaseMode::Exclusive => "only this agent",
                    agentdocker_core::LeaseMode::Shared => "shared",
                }
                .to_owned(),
            ];
            if let Some(why) = &lease.note {
                about.push(first_line(why, 160));
            }
            // Who holds it is their mark, what is held the one line of
            // text, and the time left a meter; the words for both show
            // when they are pointed at.
            let name = self.name_of(lease.holder.as_str());
            let line = row![
                tip(
                    agent_mark(
                        holder.map(|a| a.spec.runtime.as_str()),
                        &name,
                        lease.holder.as_str(),
                        18.0,
                        c
                    ),
                    about.join(" · "),
                    c
                ),
                text(resource_label(
                    &lease.resource.to_string(),
                    self.selected_root()
                ))
                .size(12)
                .font(Font::MONOSPACE)
                .color(c.text)
                .width(Fill)
                .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                tip(
                    container(meter(
                        left,
                        if low {
                            c.amber
                        } else {
                            super::style::mix(c.faint, c.text, 0.2)
                        },
                        c,
                    ))
                    .width(64)
                    .padding([6, 0]),
                    if left_secs > 0 {
                        format!("{} left", super::span(left_secs))
                    } else {
                        "Expired".to_owned()
                    },
                    c
                ),
            ]
            .spacing(12)
            .align_y(Center);
            rows = rows.push(container(line).padding([10, 16]));
        }
        if count == 0 {
            return empty(None, c);
        }
        container(rows)
            .width(Fill)
            .style(move |_| c.card_style())
            .into()
    }
}

/// A held resource as a person reads it: a path as a path — inside the
/// project on view, from its root, which the header already says;
/// elsewhere with home as `~` — anything else as `kind: value`.
fn resource_label(key: &str, root: Option<&std::path::Path>) -> String {
    match key.split_once(':') {
        Some(("path", path)) => {
            let path = std::path::Path::new(path);
            match root.and_then(|root| path.strip_prefix(root).ok()) {
                Some(inside) if !inside.as_os_str().is_empty() => inside.display().to_string(),
                _ => shorten_home(path),
            }
        }
        Some((kind, value)) => format!("{kind}: {value}"),
        None => key.to_owned(),
    }
}

/// How an ended session's undelivered messages read after "ended".
fn undelivered_phrase(count: Option<usize>) -> String {
    match count {
        Some(1) => "with 1 message not delivered".to_owned(),
        Some(n) => format!("with {n} messages not delivered"),
        None => "before its messages were delivered".to_owned(),
    }
}

/// The daemon's reason for pausing delivery, in the person's words. The
/// daemon's own text stays in the session log and the CLI; this is what the
/// app says about it. Unknown reasons are shown as they are.
pub(super) fn plain_pause_reason(reason: &str) -> String {
    use agentdocker_core::input::{
        PAUSE_CONTROLLER_ENDED, PAUSE_CONTROLLER_RESTART_FAILED, PAUSE_RECEIVER_UPGRADING,
    };
    match reason {
        PAUSE_CONTROLLER_ENDED => {
            "The helper that hands messages to this session stopped.".to_owned()
        }
        PAUSE_CONTROLLER_RESTART_FAILED => {
            "The helper that hands messages to this session could not be started again.".to_owned()
        }
        PAUSE_RECEIVER_UPGRADING => {
            "Message delivery is being updated; it resumes on its own.".to_owned()
        }
        other => other.to_owned(),
    }
}

// Questions, approvals, the Inbox without conversations and Needs you.
mod approvals;
mod changes;
mod secret_reviews;
#[cfg(test)]
pub(super) mod testing;

/// Keep full question bodies in the review screen, with a bounded first line here.
fn compact_question(value: &str) -> String {
    let first = value.lines().next().unwrap_or_default();
    let mut preview: String = first.chars().take(80).collect();
    if first.chars().count() > 80 || value.lines().count() > 1 {
        preview.push('…');
    }
    preview
}

/// Where a folder is, short enough for one line: the home folder as `~`,
/// and a deep path kept to its last two folders, to tell two folders of
/// one name apart.
fn parent_folder(path: &std::path::Path) -> String {
    let Some(parent) = path.parent() else {
        return path.display().to_string();
    };
    // The part that tells two folders of one name apart comes first, so a
    // clipped line still carries it: `agentdocker-fixture · /private/tmp`,
    // not `/private/tmp/agentd…`.
    let Some(name) = parent.file_name() else {
        return shorten_home(parent);
    };
    match parent.parent() {
        Some(above) if above.parent().is_some() => {
            let shown = shorten_home(above);
            let parts: Vec<&str> = shown.split('/').filter(|p| !p.is_empty()).collect();
            let above = if parts.len() <= 3 {
                shown
            } else {
                format!("…/{}", parts[parts.len() - 1])
            };
            format!("{} · {above}", name.to_string_lossy())
        }
        _ => name.to_string_lossy().into_owned(),
    }
}

/// A path with the home folder as `~`.
fn shorten_home(path: &std::path::Path) -> String {
    let shown = path.display().to_string();
    match std::env::var_os("HOME").map(std::path::PathBuf::from) {
        Some(home) if path.starts_with(&home) => match path.strip_prefix(&home) {
            Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => shown,
        },
        _ => shown,
    }
}

/// How a divider between panes looks: nothing until pointed at, then a
/// hairline in the accent, and the accent while it is being dragged.
pub(super) fn split_style(c: Colors) -> iced::widget::pane_grid::Style {
    use iced::widget::pane_grid::{Highlight, Line, Style};
    Style {
        hovered_region: Highlight {
            background: iced::Background::Color(iced::Color::TRANSPARENT),
            border: iced::Border::default(),
        },
        picked_split: Line {
            color: c.accent,
            width: 2.0,
        },
        hovered_split: Line {
            color: c.accent,
            width: 2.0,
        },
    }
}

#[cfg(test)]
mod tests {
    /// A role is the person's to give, from the inspector: an agent with
    /// none shows no label, the tag opens its project's roles, a pick gives
    /// or takes one, the row and the inspector show what it holds in the
    /// palette's colours and order, and the palette is edited in the same
    /// menu — a colour, a delete pressed twice, a new role by name.
    #[test]
    fn roles_are_given_from_the_inspector_and_shown_as_labels() {
        use super::super::view::testing::seen;
        use super::*;
        use agentdocker_core::{AgentSpec, AgentStatus, ProjectRef, protocol::Request};
        let (commands, requests) = crate::app::queue::channel();
        let (_messages, results) = std::sync::mpsc::sync_channel(crate::app::MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        app.connected = Ok(());
        let c = Colors::new(false);
        let mut agent = AgentRecord::new(
            AgentSpec {
                name: "reviewer-one".into(),
                runtime: "codex".into(),
                ..Default::default()
            },
            false,
            Utc::now(),
        );
        agent.status = AgentStatus::Running;
        agent.project = Some(ProjectRef::directory("/work/orchard"));
        let id = agent.id.to_string();
        app.agents.push(agent.clone());
        assert!(
            app.roles_control(&agent, c).is_none(),
            "no palette read yet, no role controls"
        );
        app.palettes_asked.insert("/work/orchard".into());
        app.palettes.insert(
            "/work/orchard".into(),
            Some(agentdocker_core::role::defaults(
                &agentdocker_core::ProjectId::from("orchard"),
                Utc::now(),
            )),
        );
        assert!(
            app.role_chips(&agent, None, c).is_none(),
            "no role, no label"
        );
        let view = seen(app.roles_control(&agent, c).expect("the tag"));
        assert_eq!(view.control("session-roles").unwrap().1, "Roles");
        assert!(view.control(&format!("role-pick-{id}-reviewer")).is_none());

        let _ = app.update(Message::RolesMenu(id.clone()));
        assert_eq!(app.shell.roles_menu.as_deref(), Some(id.as_str()));
        // The menu is an overlay over the inspector: read on its own.
        let view = seen(app.roles_menu(&agent, c));
        let pick = view
            .control(&format!("role-pick-{id}-reviewer"))
            .expect("the project's reviewer");
        assert_eq!((pick.1.as_str(), pick.3), ("Reviewer, not given", true));
        let _ = app.update(Message::RoleToggle(id.clone(), "reviewer".into()));
        let sent = requests.try_iter().collect::<Vec<_>>();
        let [crate::app::Cmd::BoardAct { project, request }] = sent.as_slice() else {
            panic!("one role change, got {sent:?}");
        };
        assert_eq!(project, "/work/orchard");
        assert!(matches!(request.as_ref(),
            Request::RoleAssign { from, agent: to, add, remove }
                if from == agentdocker_core::HUMAN && to == &id
                    && add == &["reviewer".to_owned()] && remove.is_empty()));

        // Holding two, in the palette's order whatever the label says.
        agent.spec.labels.insert(
            agentdocker_core::agent::ROLE_LABEL.into(),
            "tester,reviewer".into(),
        );
        app.agents[0] = agent.clone();
        assert_eq!(app.role_names(&agent), ["Reviewer", "Tester"]);
        let row = seen(app.role_chips(&agent, Some(1), c).unwrap());
        assert!(row.reads("Reviewer") && row.reads("+1") && !row.reads("Tester"));
        let view = seen(app.roles_control(&agent, c).unwrap());
        assert_eq!(
            view.control("session-roles").unwrap().1,
            "Roles: Reviewer, Tester"
        );
        assert!(view.reads("Reviewer") && view.reads("Tester"), "its labels");
        let view = seen(app.roles_menu(&agent, c));
        assert_eq!(
            view.control(&format!("role-pick-{id}-tester")).unwrap().1,
            "Tester, given"
        );
        let _ = app.update(Message::RoleToggle(id.clone(), "tester".into()));
        assert!(matches!(requests.try_iter().collect::<Vec<_>>().as_slice(),
            [crate::app::Cmd::BoardAct { request, .. }]
                if matches!(request.as_ref(), Request::RoleAssign { add, remove, .. }
                    if add.is_empty() && remove == &["tester".to_owned()])));

        // The palette, edited in the same menu.
        let _ = app.update(Message::RolesEdit);
        let view = seen(app.roles_menu(&agent, c));
        assert_eq!(
            view.control("role-color-reviewer").unwrap().1,
            "Reviewer, orange"
        );
        assert!(view.control("role-delete-reviewer").is_some());
        assert!(view.control("role-new").is_some());
        let _ = app.update(Message::RoleRecolor("reviewer".into()));
        assert!(matches!(requests.try_iter().collect::<Vec<_>>().as_slice(),
            [crate::app::Cmd::BoardAct { request, .. }]
                if matches!(request.as_ref(), Request::RoleSave { role: Some(role), name, color, project, .. }
                    if role == "reviewer" && name == "Reviewer"
                        && *color == Some(agentdocker_core::RoleColor::Pink)
                        && project.as_deref() == Some("/work/orchard"))));
        // A delete takes the role from every agent holding it: pressed once
        // it asks, saying how many; twice, it goes.
        let _ = app.update(Message::RoleDelete("reviewer".into()));
        assert_eq!(requests.try_iter().count(), 0);
        let view = seen(app.roles_menu(&agent, c));
        assert_eq!(
            view.control("role-delete-reviewer").unwrap().1,
            "Delete Reviewer: 1 agent loses it"
        );
        let _ = app.update(Message::RoleDelete("reviewer".into()));
        assert!(matches!(requests.try_iter().collect::<Vec<_>>().as_slice(),
            [crate::app::Cmd::BoardAct { request, .. }]
                if matches!(request.as_ref(), Request::RoleDelete { role, .. } if role == "reviewer")));
        let _ = app.update(Message::RoleDraft("Docs".into()));
        let _ = app.update(Message::RoleCreate);
        assert!(matches!(requests.try_iter().collect::<Vec<_>>().as_slice(),
            [crate::app::Cmd::BoardAct { request, .. }]
                if matches!(request.as_ref(), Request::RoleSave { role: None, name, .. } if name == "Docs")));
        assert!(app.shell.role_draft.is_empty());

        // A palette emptied of every role keeps the menu, so one can be
        // made again; a palette never read shows nothing, labels included.
        app.palettes
            .insert("/work/orchard".into(), Some(Vec::new()));
        assert!(app.roles_control(&agent, c).is_some(), "the menu stays");
        app.palettes.insert("/work/orchard".into(), None);
        assert!(app.roles_control(&agent, c).is_none());
        assert!(
            app.role_chips(&agent, None, c).is_none() && app.role_names(&agent).is_empty(),
            "a role label with no palette to read it against is not shown"
        );

        // The person is given no roles.
        let mut person = agent.clone();
        person.spec.runtime = agentdocker_core::HUMAN_RUNTIME.into();
        assert!(app.roles_control(&person, c).is_none());
    }

    /// A healthy service says nothing at the rail's foot; a lost connection
    /// or an update to install is one line there, and the bottom strip is
    /// gone unless a restart is being confirmed.
    #[test]
    fn the_rail_says_nothing_of_a_healthy_service_and_one_thing_otherwise() {
        use super::*;
        let (commands, _requests) = crate::app::queue::channel();
        let (_messages, results) = std::sync::mpsc::sync_channel(crate::app::MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        let c = Colors::new(true);
        let r = c.rail();
        app.connected = Ok(());
        assert!(app.service_chip(r).is_none(), "healthy: nothing");
        assert!(app.daemon_notice(c).is_none(), "and no bottom strip");
        app.desktop.update = Some(serde_json::json!({
            "update_available": true,
            "available": {"version": "0.2.1"}
        }));
        assert!(app.service_chip(r).is_some(), "an update to install");
        app.desktop.update = None;
        app.connected = Err("gone".into());
        assert!(app.service_chip(r).is_some(), "reconnecting");
    }

    /// A limit's reset reads on the person's clock: minutes, then hours
    /// and minutes, then a day; a past one says so.
    #[test]
    fn a_provider_reset_reads_in_local_time() {
        use chrono::TimeZone;
        let zone = chrono::FixedOffset::west_opt(7 * 3600).unwrap();
        let now = chrono::Utc
            .with_ymd_and_hms(2026, 10, 15, 21, 18, 0)
            .unwrap();
        let at = |minutes: i64| now + chrono::Duration::minutes(minutes);
        assert_eq!(
            super::reset_words(now, at(42), &zone),
            "resets in 42 min (15:00)"
        );
        assert_eq!(
            super::reset_words(now, at(190), &zone),
            "resets in 3 h 10 min (17:28)"
        );
        assert_eq!(
            super::reset_words(now, at(120), &zone),
            "resets in 2 h (16:18)"
        );
        assert_eq!(
            super::reset_words(now, at(26 * 60), &zone),
            "resets Oct 16, 16:18"
        );
        assert_eq!(
            super::reset_words(now, at(-5), &zone),
            "should have reset by now"
        );
    }

    #[test]
    fn a_held_path_reads_from_the_project_root_on_view() {
        let root = std::path::Path::new("/work/project");
        assert_eq!(
            super::resource_label("path:/work/project/crates/ui/src/app/board.rs", Some(root)),
            "crates/ui/src/app/board.rs"
        );
        assert_eq!(
            super::resource_label("path:/elsewhere/notes.md", Some(root)),
            "/elsewhere/notes.md"
        );
        assert_eq!(
            super::resource_label("path:/work/project", Some(root)),
            "/work/project",
            "the root itself is said whole"
        );
        assert_eq!(
            super::resource_label("task:abc123", Some(root)),
            "task: abc123"
        );
        assert_eq!(super::resource_label("plain", None), "plain");
    }

    #[test]
    fn a_shared_folder_name_is_told_apart_by_its_parent_first() {
        assert_eq!(
            super::parent_folder(std::path::Path::new(
                "/private/tmp/agentdocker-fixture/workspace"
            )),
            "agentdocker-fixture · /private/tmp"
        );
        assert_eq!(
            super::parent_folder(std::path::Path::new("/srv/workspace")),
            "srv"
        );
    }

    #[test]
    fn daemon_pause_reasons_read_in_the_persons_words() {
        use agentdocker_core::input::{
            PAUSE_CONTROLLER_ENDED, PAUSE_CONTROLLER_RESTART_FAILED, PAUSE_RECEIVER_UPGRADING,
        };
        for reason in [
            PAUSE_CONTROLLER_ENDED,
            PAUSE_CONTROLLER_RESTART_FAILED,
            PAUSE_RECEIVER_UPGRADING,
        ] {
            let plain = super::plain_pause_reason(reason);
            assert_ne!(plain, reason);
            assert!(!plain.contains("controller") && !plain.contains("receiver"));
        }
        assert_eq!(super::plain_pause_reason("something new"), "something new");
        assert_eq!(
            super::undelivered_phrase(Some(1)),
            "with 1 message not delivered"
        );
        assert_eq!(
            super::undelivered_phrase(Some(3)),
            "with 3 messages not delivered"
        );
    }

    use super::{remaining_fraction, spoken_payload};
    use chrono::{Duration, Utc};
    use serde_json::json;

    #[test]
    fn tools_require_session_contact_and_never_infer_input_from_general_activity() {
        use agentdocker_core::{
            AdapterContact, AdapterKind, AgentRecord, AgentSpec, AgentStatus, InputDelivery,
        };
        let (tx, _commands) = crate::app::queue::channel();
        let (_sender, rx) = std::sync::mpsc::channel();
        let mut app = crate::app::App::bare(tx, rx);
        let now = Utc::now();
        let birth = now - Duration::seconds(10);
        let mut agent = AgentRecord::new(
            AgentSpec {
                runtime: "codex".into(),
                ..Default::default()
            },
            false,
            birth,
        );
        agent.status = AgentStatus::Running;
        agent.process_started_at = Some(birth);
        app.activity.insert(
            agent.id.to_string(),
            agentdocker_core::Activity::Working { since: now },
        );
        app.agents.push(agent.clone());
        assert!(
            !app.tool_reports("codex"),
            "leases and coordination are not adapter contact"
        );
        assert_eq!(
            app.input_readiness(&agent).label(),
            "Messages may wait for its next prompt"
        );
        agent.adapter_contacts.insert(
            AdapterKind::Mcp,
            AdapterContact {
                process_started_at: birth,
                observed_at: now,
            },
        );
        app.agents[0] = agent.clone();
        assert!(app.tool_reports("codex"));
        assert!(
            !app.tool_reports("claude-code"),
            "contact is specific to the runtime"
        );
        assert_eq!(
            app.input_readiness(&agent).label(),
            "Messages may wait for its next prompt",
            "MCP contact does not prove idle wake"
        );
        agent.input_delivery = Some(InputDelivery {
            process_started_at: birth,
            paused: false,
            pause_reason: None,
            reported_at: now,
            received: None,
            received_at: None,
            typing_until: None,
        });
        assert_eq!(app.input_readiness(&agent).label(), "Ready for messages");
        // Words queued behind a current receiver that no receipt covers are
        // waiting on the provider; an earlier receipt does not make them
        // delivered, and a receipt for them does, acknowledged or not.
        app.queued_inputs.insert(agent.id.to_string(), 2);
        app.awaiting_receipt.insert(agent.id.to_string(), 2);
        assert_eq!(
            app.input_readiness(&agent).label(),
            "Sent · waiting for the agent to take it"
        );
        agent.input_delivery.as_mut().unwrap().received = Some(agentdocker_core::ReceivedInput {
            messages: vec!["m1".to_owned().into()],
            receipt: agentdocker_core::InputReceipt::ClaudeChannel,
        });
        agent.input_delivery.as_mut().unwrap().received_at = Some(now);
        app.awaiting_receipt.insert(agent.id.to_string(), 1);
        assert_eq!(
            app.input_readiness(&agent).label(),
            "Sent · waiting for the agent to take it"
        );
        app.awaiting_receipt.insert(agent.id.to_string(), 0);
        assert_eq!(app.input_readiness(&agent).label(), "Receiving messages");
        agent.input_delivery.as_mut().unwrap().received = None;
        agent.input_delivery.as_mut().unwrap().received_at = None;
        app.queued_inputs.remove(agent.id.as_str());
        app.awaiting_receipt.remove(agent.id.as_str());
        agent.process_started_at = Some(now);
        app.agents[0] = agent.clone();
        assert!(
            !app.tool_reports("codex"),
            "a new process cannot inherit contact"
        );
        assert_eq!(
            app.input_readiness(&agent).label(),
            "Not heard from recently"
        );
        agent.process_started_at = Some(birth);
        agent.input_delivery.as_mut().unwrap().paused = true;
        assert_eq!(
            app.input_readiness(&agent).label(),
            "Not receiving messages"
        );
        app.connected = Err("offline".into());
        assert!(!app.tool_reports("codex"));
        assert_eq!(app.input_readiness(&agent).label(), "Readiness unavailable");
    }

    #[test]
    fn a_window_drains_from_one_to_zero_and_never_beyond() {
        let start = Utc::now();
        let end = start + Duration::seconds(100);
        assert_eq!(remaining_fraction(start, end, start), 1.0);
        assert!((remaining_fraction(start, end, start + Duration::seconds(50)) - 0.5).abs() < 1e-3);
        assert_eq!(remaining_fraction(start, end, end), 0.0);
        assert_eq!(
            remaining_fraction(start, end, end + Duration::seconds(5)),
            0.0
        );
        assert_eq!(
            remaining_fraction(start, end, start - Duration::seconds(5)),
            1.0
        );
        // A window with no length, or one that ends before it starts, is over.
        assert_eq!(remaining_fraction(start, start, start), 0.0);
        assert_eq!(remaining_fraction(end, start, start), 0.0);
    }

    /// The controls an element offers, in the order drawn: each one's id,
    /// its label and what pressing it sends.
    fn offered(
        element: iced::Element<'_, crate::app::Message>,
    ) -> Vec<(String, String, Option<crate::app::Message>)> {
        inspected(element).controls
    }

    #[derive(Default)]
    struct Inspected {
        controls: Vec<(String, String, Option<crate::app::Message>)>,
        text: Vec<String>,
    }

    impl iced::advanced::widget::Operation for Inspected {
        fn traverse(
            &mut self,
            operate: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation),
        ) {
            operate(self);
        }
        fn text(&mut self, _: Option<&iced::advanced::widget::Id>, _: iced::Rectangle, text: &str) {
            self.text.push(text.to_owned());
        }
        fn custom(
            &mut self,
            _: Option<&iced::advanced::widget::Id>,
            _: iced::Rectangle,
            state: &mut dyn std::any::Any,
        ) {
            if let Some(semantic) = state.downcast_ref::<crate::accessibility::Semantic>() {
                self.controls.push((
                    semantic.id.clone(),
                    semantic.label.clone(),
                    semantic.action.clone(),
                ));
            }
        }
        fn finish(&self) -> iced::advanced::widget::operation::Outcome<()> {
            iced::advanced::widget::operation::Outcome::None
        }
    }

    /// Inspect actual text and accessibility controls in the laid-out widget tree.
    fn inspected(mut element: iced::Element<'_, crate::app::Message>) -> Inspected {
        use iced::advanced::widget::Tree;
        use iced::advanced::{Layout, layout};
        let renderer = iced::Renderer::new(iced::Font::DEFAULT, 14.0.into());
        let mut tree = Tree::new(&element);
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(iced::Size::ZERO, iced::Size::new(900.0, 600.0)),
        );
        let mut offered = Inspected::default();
        element
            .as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut offered);
        offered
    }

    /// Dependencies outside the current page still govern both the rendered
    /// Start action and an action queued before a newer detail arrived.
    #[test]
    fn board_start_uses_complete_detail_and_rechecks_stale_actions() {
        use crate::app::shell::{BoardLayout, CardMenu};
        use crate::app::{CardDetail, Cmd, Message, Msg, Screen};
        use agentdocker_core::{Column, Request};
        let (tx, commands) = crate::app::queue::channel();
        let (messages, rx) = std::sync::mpsc::sync_channel(crate::app::MESSAGE_CAPACITY);
        let mut app = crate::app::App::bare(tx, rx);
        let dir = tempfile::tempdir().unwrap();
        let project = agentdocker_core::ProjectRef::directory(dir.path());
        let root = project.root.display().to_string();
        app.shell.catalog.remember(project.clone(), true).unwrap();
        app.shell.catalog.selected = Some(project.root.clone());
        app.connected = Ok(());
        app.screen = Screen::Board;
        app.shell.board_layout = BoardLayout::Table;
        let now = chrono::Utc::now();
        let card = |id: &str| -> agentdocker_core::Task {
            serde_json::from_value(serde_json::json!({
                "id": id, "project": project.id(), "title": id,
                "acceptance": "", "column": "ready", "assignee": null,
                "created_by": "user", "created_at": now, "updated_at": now,
                "archived_at": null, "links": []
            }))
            .unwrap()
        };
        let blocker = card("bbbbbbbbbbbb");
        let mut dependent = card("aaaaaaaaaaaa");
        dependent.blocked_by.push(blocker.id.clone());
        app.request_tasks();
        let request = commands
            .try_iter()
            .find_map(|c| match c {
                Cmd::Tasks { request, .. } => Some(request),
                _ => None,
            })
            .unwrap();
        messages
            .send(Msg::Tasks(
                root,
                request,
                Ok((vec![dependent.clone()], true)),
            ))
            .unwrap();
        app.drain();
        let _ = app.update(Message::TaskOpen(dependent.id.clone()));
        let detail = CardDetail {
            task: dependent.clone(),
            board: agentdocker_core::Board::main(project.id(), "test", now),
            comments: vec![],
            history: vec![],
            blockers: vec![blocker],
            children: vec![],
            plan: None,
            files: vec![],
        };
        messages
            .send(Msg::TaskDetail(
                dependent.id.clone(),
                Ok(Box::new(detail.clone())),
            ))
            .unwrap();
        app.drain();
        let start = |app: &crate::app::App| {
            offered(app.board_view(super::Colors::new(false)))
                .into_iter()
                .find(|(id, _, _)| id == "card-start")
        };
        assert!(app.tasks.as_ref().unwrap().more);
        assert_eq!(app.tasks.as_ref().unwrap().cards.len(), 1);
        assert!(
            start(&app).is_none(),
            "an open prerequisite on a later page blocks Start"
        );
        let take_starts = || {
            commands
                .try_iter()
                .filter_map(|c| match c {
                    Cmd::BoardAct { request, .. } => match *request {
                        Request::TaskStart { task, .. } => Some(task),
                        _ => None,
                    },
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let _ = app.update(Message::CardMenu(CardMenu::Start));
        assert_eq!(app.shell.card_menu, None);
        let _ = app.update(Message::StartAgent("codex".into()));
        assert!(take_starts().is_empty());

        // A closed prerequisite permits the action even though the board page
        // still has the old card. Use the actual rendered button's callback.
        app.card.as_mut().unwrap().blockers[0].column = Column::Done;
        let (_, _, action) = start(&app).expect("closed prerequisite permits Start");
        let _ = app.update(action.unwrap());
        assert_eq!(app.shell.card_menu, Some(CardMenu::Start));
        let _ = app.update(Message::StartAgent("codex".into()));
        assert_eq!(take_starts(), vec![dependent.id.to_string()]);

        // A queued click must not bypass a newer blocker or a changed card.
        let _ = app.update(Message::CardMenu(CardMenu::Start));
        app.card.as_mut().unwrap().blockers[0].column = Column::Ready;
        assert!(start(&app).is_none());
        let _ = app.update(Message::StartAgent("codex".into()));
        assert!(take_starts().is_empty());
        assert_eq!(app.shell.card_menu, None);
        app.card.as_mut().unwrap().blockers.clear();
        assert!(
            start(&app).is_none(),
            "missing dependency details are not proof of completion"
        );
        app.card.as_mut().unwrap().task.id = "cccccccccccc".to_owned().into();
        assert!(
            start(&app).is_none(),
            "another card's detail cannot authorize this one"
        );
        let _ = app.update(Message::StartAgent("codex".into()));
        assert!(take_starts().is_empty());
        app.card = None;
        assert!(start(&app).is_none(), "wait for the opened card's detail");
        let _ = app.update(Message::StartAgent("codex".into()));
        assert!(take_starts().is_empty());

        // The detail's own task fields also take precedence over the page.
        app.card = Some(detail);
        app.card.as_mut().unwrap().task.blocked_by.clear();
        app.card.as_mut().unwrap().blockers.clear();
        assert!(start(&app).is_some());
        app.card.as_mut().unwrap().task.external_blocker = "approval".into();
        assert!(start(&app).is_none());
        let _ = app.update(Message::StartAgent("codex".into()));
        assert!(take_starts().is_empty());
    }

    #[test]
    fn an_operation_blocks_empty_review_cleanup_even_when_the_target_is_detached() {
        use agentdocker_core::review::{Operation, Review};
        let (tx, _commands) = crate::app::queue::channel();
        let (_sender, rx) = std::sync::mpsc::channel();
        let mut app = crate::app::App::bare(tx, rx);
        let c = super::Colors::new(false);
        let mut agent = agentdocker_core::AgentRecord::new(
            agentdocker_core::AgentSpec::default(),
            false,
            Utc::now(),
        );
        agent.status = agentdocker_core::AgentStatus::Exited { code: Some(0) };
        let mut review = Review {
            agent: agent.id.clone(),
            checkout: "repo/worktree".into(),
            target_checkout: "repo".into(),
            branch: Some("feature".into()),
            target: Some("main".into()),
            ..Review::default()
        };
        // A truly empty branch still offers the person's cleanup action.
        app.branch_review = Some((agent.id.to_string(), Ok(review.clone())));
        let controls = offered(app.changes_section(&agent, c).unwrap());
        assert!(controls.iter().any(|(id, _, _)| id == "remove-worktree"));
        let reason = "Git did not confirm the merge: the pre-merge-commit hook rejected it";
        app.merge_outcome = Some((agent.id.to_string(), Err(reason.to_owned())));
        for (operation, target) in [
            (Operation::Merge, Some("main")),
            (Operation::Rebase, None),
            (Operation::CherryPick, Some("main")),
        ] {
            review.in_progress = Some(operation);
            review.target = target.map(str::to_owned);
            app.branch_review = Some((agent.id.to_string(), Ok(review.clone())));
            let shown = inspected(app.changes_section(&agent, c).unwrap());
            // The marks beside the blocker and the failure are named labels,
            // not actions: the only action is Refresh.
            let ids: Vec<&str> = shown
                .controls
                .iter()
                .map(|(id, _, _)| id.as_str())
                .collect();
            assert!(
                ids.contains(&"review-refresh")
                    && !ids
                        .iter()
                        .any(|id| *id == "remove-worktree" || *id == "merge-branch"),
                "pending operation must not offer worktree removal: {operation:?} {ids:?}"
            );
            assert!(
                shown.text.iter().any(|line| line == reason),
                "the merge failure must remain visible beside its operation blocker"
            );
            assert!(
                shown
                    .text
                    .iter()
                    .any(|line| line.contains(operation.named())),
                "the actual operation reason is rendered: {:?}",
                shown.text
            );
            assert!(
                !shown
                    .text
                    .iter()
                    .any(|line| line.starts_with("Nothing on ")),
                "an operation is not an empty branch"
            );
        }
    }

    /// Open in… is there only with an editor installed: one opens the
    /// folder straight in it (*Open in Zed*), several open a menu listing
    /// the one chosen last first, each row opening the folder it was built
    /// for. A session's opens the worktree it works in, and only a
    /// worktree of its own: the project's folder is the header's.
    #[test]
    fn open_in_offers_the_installed_editors_the_last_choice_first() {
        use crate::app::Message;
        use crate::app::shell::EditorMenu;
        use agentdocker_core::editor::from_bundles;
        use std::path::{Path, PathBuf};
        let (tx, _commands) = crate::app::queue::channel();
        let (_sender, rx) = std::sync::mpsc::channel();
        let mut app = crate::app::App::bare(tx, rx);
        let c = super::Colors::new(false);
        let folder = Path::new("/work/alpha");
        assert!(
            app.open_in(EditorMenu::Project, folder, true, c).is_none(),
            "no editor, no control"
        );

        app.shell.editors = from_bundles([PathBuf::from("/Applications/Zed.app")]);
        let only = offered(app.open_in(EditorMenu::Project, folder, true, c).unwrap());
        assert!(
            matches!(
                only.as_slice(),
                [(id, label, Some(Message::OpenIn(kind, to)))]
                    if id == "project-open-in" && label == "Open in Zed" && kind == "zed" && to == folder
            ),
            "{only:?}"
        );

        app.shell.editors = from_bundles(
            [
                "/Applications/Zed.app",
                "/Applications/RustRover.app",
                "/Applications/Visual Studio Code.app",
            ]
            .map(PathBuf::from),
        );
        let trigger = offered(app.open_in(EditorMenu::Session, folder, true, c).unwrap());
        assert!(
            matches!(
                trigger.as_slice(),
                [(id, label, Some(Message::EditorMenu(EditorMenu::Session)))]
                    if id == "session-open-in" && label == "Open in…"
            ),
            "{trigger:?}"
        );
        app.shell.catalog.editor = Some("rustrover".into());
        let rows = offered(app.open_in_menu(folder, true, c));
        let listed: Vec<(&str, &str)> = rows
            .iter()
            .map(|(id, label, _)| (id.as_str(), label.as_str()))
            .collect();
        assert_eq!(
            listed,
            [
                ("open-in-rustrover", "Open in RustRover"),
                ("open-in-vscode", "Open in Visual Studio Code"),
                ("open-in-zed", "Open in Zed"),
            ]
        );
        assert!(rows.iter().all(|(id, _, action)| matches!(
            action,
            Some(Message::OpenIn(kind, to)) if *id == format!("open-in-{kind}") && to == folder
        )));
        assert!(
            offered(app.open_in_menu(folder, false, c))
                .iter()
                .all(|(_, _, action)| action.is_none()),
            "a folder that is unavailable opens in nothing"
        );

        let worktree = Path::new("/work/alpha-worktrees/feature");
        let mut agent = agentdocker_core::AgentRecord::new(
            agentdocker_core::AgentSpec {
                workdir: Some(worktree.to_owned()),
                ..Default::default()
            },
            false,
            Utc::now(),
        );
        assert_eq!(crate::app::App::session_editor_folder(&agent), None);
        agent.project = Some(agentdocker_core::ProjectRef::directory("/work/alpha"));
        assert_eq!(
            crate::app::App::session_editor_folder(&agent),
            None,
            "a session in the project's own folder"
        );
        agent.project.as_mut().unwrap().worktree = Some(worktree.to_owned());
        assert_eq!(
            crate::app::App::session_editor_folder(&agent),
            Some(worktree)
        );
    }

    /// Every state is a chip that prints its word, the healthy ones too:
    /// a project row in the rail says its loudest agent's standing (and
    /// how many need the person), a session row and the session's panel
    /// say its standing and its readiness, and a row waiting on the person
    /// prints the time left to answer.
    #[test]
    fn rows_say_their_states_in_chips() {
        use super::super::view::testing::seen;
        use super::*;
        use agentdocker_core::{Activity, AgentSpec, AgentStatus, ProjectRef};
        let (commands, _requests) = crate::app::queue::channel();
        let (_messages, results) = std::sync::mpsc::sync_channel(crate::app::MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        app.connected = Ok(());
        let root = std::path::PathBuf::from("/work/orchard");
        app.shell.catalog.projects.push(crate::catalog::Entry {
            project: ProjectRef::directory(&root),
            pinned: true,
            label: None,
        });
        let agent = |name: &str| {
            let mut agent = AgentRecord::new(
                AgentSpec {
                    name: name.into(),
                    runtime: "codex".into(),
                    ..Default::default()
                },
                false,
                Utc::now(),
            );
            agent.status = AgentStatus::Running;
            agent.project = Some(ProjectRef::directory(&root));
            agent
        };
        let (worker, quiet) = (agent("worker"), agent("quiet"));
        // No fresh report (Unknown) and no report at all: neither is idle.
        let (stale, silent) = (agent("stale"), agent("silent"));
        app.activity.insert(
            worker.id.to_string(),
            Activity::Working { since: Utc::now() },
        );
        app.activity
            .insert(quiet.id.to_string(), Activity::Idle { since: Utc::now() });
        app.activity.insert(stale.id.to_string(), Activity::Unknown);
        app.agents = vec![worker.clone(), quiet.clone(), stale.clone(), silent.clone()];
        let c = Colors::new(false);
        let label = |view: &super::super::view::testing::Seen, id: &str| {
            view.control(id)
                .unwrap_or_else(|| panic!("no {id} in {:?}", view.controls))
                .1
                .clone()
        };

        let rail = seen(app.sidebar(c));
        let project = format!("project-standing-{}", root.display());
        assert_eq!(label(&rail, &project), "Working", "the loudest, healthy");
        assert!(rail.texts.iter().any(|t| t == "Working"), "printed");

        let rows = seen(app.session_rows(&[&worker, &quiet], c));
        assert_eq!(
            label(&rows, &format!("session-standing-{}", worker.id)),
            "Working"
        );
        assert_eq!(
            label(&rows, &format!("session-standing-{}", quiet.id)),
            "Idle",
            "an idle session says so too"
        );
        let rows = seen(app.session_rows(&[&stale, &silent], c));
        for agent in [&stale, &silent] {
            assert_eq!(
                label(&rows, &format!("session-standing-{}", agent.id)),
                "Running",
                "{}: live without a report is not idle",
                agent.spec.name
            );
        }
        // A project's chip is its loudest agent's: idle only when an
        // adapter said so, running when nothing did.
        let all = app.agents.clone();
        for (only, word) in [(&quiet, "Idle"), (&stale, "Running"), (&silent, "Running")] {
            app.agents = vec![only.clone()];
            assert_eq!(
                label(&seen(app.sidebar(c)), &project),
                word,
                "{}",
                only.spec.name
            );
        }
        app.agents = all;

        let panel = seen(app.inspector(&worker, c));
        assert_eq!(label(&panel, "inspector-standing"), "Working");
        let readiness = app.input_readiness(&worker);
        assert_eq!(label(&panel, "inspector-readiness"), readiness.label());

        // Two that need the person: the project's chip says so and counts.
        let asked = |from: &AgentRecord, id: &str| agentdocker_core::Question {
            id: agentdocker_core::MessageId::from(id.to_owned()),
            from: from.id.to_string(),
            to: agentdocker_core::Destination::Agent("human".into()),
            text: "Which route?".into(),
            presentation: None,
            asked_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(10),
        };
        app.questions = vec![asked(&worker, "q1"), asked(&quiet, "q2")];
        let rail = seen(app.sidebar(c));
        assert_eq!(label(&rail, &project), "Needs you · 2");
        let rows = seen(app.session_rows(&[&worker], c));
        assert_eq!(
            label(&rows, &format!("session-standing-{}", worker.id)),
            "Needs you"
        );
        assert!(
            label(&rows, &format!("session-time-left-{}", worker.id)).ends_with("m left"),
            "the time left to answer is printed"
        );
    }

    /// A pull request's chip prints its number and its checks in a word.
    #[test]
    fn a_pull_request_chip_says_its_checks() {
        use crate::pull_request::{PullRequest, Status};
        let words = |status| {
            super::pull_request_look(&PullRequest {
                number: 12,
                url: "https://example.invalid/pull/12".into(),
                status,
                head: None,
            })
            .words
        };
        assert_eq!(words(Status::Passed), "#12 · Passed");
        assert_eq!(words(Status::Running), "#12 · Running");
        assert_eq!(words(Status::Failed), "#12 · Failed");
        assert_eq!(words(Status::Merged), "#12 · Merged");
        assert_eq!(words(Status::Open), "#12 · Open");
    }

    /// Words on screen: the restart strip is its question and what it
    /// costs as counts, with nothing said to reassure; the rename field's
    /// placeholder is the name an emptied field goes back to, with no line
    /// saying so, and the menu has no path at its foot; the launch form's
    /// field is named Task, not asked for in a sentence.
    #[test]
    fn the_strip_the_rename_and_the_launch_form_carry_no_helper_lines() {
        use super::super::view::testing::seen;
        use super::*;
        use agentdocker_core::{AgentSpec, AgentStatus, ProjectRef};
        let (commands, _requests) = crate::app::queue::channel();
        let (_messages, results) = std::sync::mpsc::sync_channel(crate::app::MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        app.connected = Ok(());
        let c = Colors::new(false);

        app.daemon = Some(crate::app::DaemonInfo {
            version: "0.0.1".into(),
            executable: None,
        });
        app.shell.confirm_daemon_restart = true;
        let strip = seen(app.daemon_notice(c).expect("the strip"));
        assert!(
            strip
                .texts
                .iter()
                .any(|t| t == "Restart the background service?"),
            "nothing launched: the question alone: {:?}",
            strip.texts
        );
        for gone in ["keep running", "on their own", "started in a terminal"] {
            assert!(!strip.reads(gone), "no reassurance: {gone}");
        }
        let mut launched = AgentRecord::new(
            AgentSpec {
                name: "launched".into(),
                runtime: "codex".into(),
                restore: true,
                ..Default::default()
            },
            true,
            Utc::now(),
        );
        launched.status = AgentStatus::Running;
        app.agents.push(launched);
        let strip = seen(app.daemon_notice(c).expect("the strip"));
        assert!(
            strip
                .reads("Restart the background service? 1 launched session stops, 1 starts again."),
            "{:?}",
            strip.texts
        );
        assert!(!strip.reads("keep running"));
        app.shell.confirm_daemon_restart = false;

        let project = ProjectRef::directory("/fixture/alpha");
        app.shell.catalog.remember(project.clone(), false).unwrap();
        app.shell.project_rename = Some((project.root.clone(), String::new()));
        let entry = app.shell.catalog.projects[0].clone();
        let menu = seen(app.project_menu(&entry, c));
        let key = project.root.display().to_string();
        let field = menu
            .control(&format!("project-rename-{key}"))
            .expect("the name field");
        assert_eq!(
            field.1, "alpha",
            "the placeholder is the folder's name, what an empty name goes back to"
        );
        assert!(
            menu.control(&format!("project-rename-save-{key}"))
                .is_some()
        );
        assert!(!menu.reads("Empty goes back"));
        // The folder is the row mark's, on the pointer and to a reader,
        // not the menu's foot.
        assert!(!menu.texts.iter().any(|t| t.contains("/fixture/alpha")));
        let shared = std::collections::BTreeSet::new();
        let row = seen(app.project_row(&entry, &shared, true, c));
        assert_eq!(
            row.control(&format!("project-mark-{key}"))
                .map(|m| m.1.as_str()),
            Some("/fixture/alpha"),
            "the mark names its folder without the pointer"
        );
        app.shell.project_rename = None;
        let menu = seen(app.project_menu(&entry, c));
        assert!(menu.control(&format!("project-remove-{key}")).is_some());
        assert!(!menu.texts.iter().any(|t| t.contains("/fixture/alpha")));

        app.shell.catalog.selected = Some(project.root.clone());
        app.runtimes = vec![agentdocker_core::runtime::RuntimeInfo {
            name: "codex".into(),
            vendor: "fixture".into(),
            label: "Codex".into(),
            cli: Some("/fixture/codex".into()),
            version: None,
            apps: vec![],
            extensions: vec![],
            incomplete: vec![],
            config_dir: None,
            mcp: agentdocker_core::runtime::Wiring::Wired,
            hooks: agentdocker_core::runtime::Wiring::Wired,
            hooks_missing: vec![],
            shell: agentdocker_core::runtime::Wiring::Unsupported,
            running: 0,
        }];
        app.shell.launch = true;
        app.shell.launch_runtime = Some("codex".into());
        let form = seen(app.launch_view(c));
        let task = form
            .control(super::super::shell::LAUNCH_PROMPT)
            .expect("the task field");
        assert!(task.1.starts_with("Task "), "named Task: {}", task.1);
        assert!(!form.reads("What should it work on"));
        assert!(form.control("confirm-launch").is_some());

        // An ended session's last message is a fact under Details, not a
        // line of its own in the panel.
        let now = Utc::now();
        let mut ended = AgentRecord::new(
            AgentSpec {
                name: "ended".into(),
                runtime: "codex".into(),
                ..Default::default()
            },
            false,
            now,
        );
        ended.status = AgentStatus::Exited { code: Some(0) };
        ended.input_delivery = Some(agentdocker_core::InputDelivery {
            process_started_at: now,
            paused: false,
            pause_reason: None,
            reported_at: now,
            received: None,
            received_at: Some(now),
            typing_until: None,
        });
        let panel = seen(app.inspector(&ended, c));
        assert!(!panel.reads("Last took a message"));
        assert!(!panel.reads("Last message"), "behind Details until opened");
        app.shell.session_details = true;
        let panel = seen(app.inspector(&ended, c));
        assert!(panel.reads("Last message"));
    }

    #[test]
    fn a_message_shows_its_text_and_only_textless_payloads_show_json() {
        assert_eq!(spoken_payload(&json!("plain")), "plain");
        assert_eq!(
            spoken_payload(&json!({"text": "hello", "channel": "c1"})),
            "hello"
        );
        assert!(spoken_payload(&json!({"verdict": "approve"})).contains("\"verdict\""));
    }
}
