//! Projects, attention and contextual tools over live daemon state.
//!
//! The window is a rail and a workspace. The rail carries the mark, the
//! four destinations and the project list; the workspace leads with the
//! project name, then the section tabs, then whatever the section shows.
//! Status is drawn as a coloured dot *and* said in words, every time.
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
/// row: the kind as a quiet pill, the target as text to read, and the
/// note after it. Nothing is opened or copied from here — a path or a
/// pull request is the reader's to open with their own tools; what the
/// row does is say what kind of thing it is without prose.
pub(super) fn links<'a>(links: &[agentdocker_core::Link], c: Colors) -> Element<'a, Message> {
    let mut rows = column![].spacing(3).width(Fill);
    for link in links {
        let mut line = row![
            pill(link.kind.as_str().to_owned(), c.raised, c.muted, c),
            text(link.target.clone())
                .size(12)
                .color(c.accent)
                .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
        ]
        .spacing(6)
        .align_y(Center);
        if let Some(note) = &link.note {
            line = line.push(small(note.clone(), c));
        }
        rows = rows.push(line);
    }
    rows.into()
}

pub(super) fn small<'a>(value: impl Into<String>, c: Colors) -> iced::widget::Text<'a> {
    text(value.into()).size(12).color(c.muted)
}
/// A section label: short, quiet, set in capitals.
pub(super) fn eyebrow<'a>(value: impl Into<String>, c: Colors) -> iced::widget::Text<'a> {
    text(value.into().to_uppercase())
        .size(11)
        .color(c.faint)
        .font(weight(iced::font::Weight::Semibold))
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
/// Something small that says what it means when pointed at. Used only
/// where the words are not already printed beside it (the rail's project
/// marks); a tooltip that repeats visible text is noise.
fn hint<'a>(
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
/// The strip along the top of a pane: what it holds, and one quiet fact.
fn pane_header<'a>(
    title_text: &'a str,
    meta: impl Into<String>,
    c: Colors,
) -> Element<'a, Message> {
    column![
        container(
            row![
                eyebrow(title_text, c).width(Fill),
                small(meta, c).color(c.faint)
            ]
            .spacing(12)
            .align_y(Center),
        )
        .padding([10, 16]),
        rule(c)
    ]
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
/// AgentDocker's own cube on a quiet tile: what speaks for the app
/// itself, such as the notices it sends.
pub(super) fn brand_tile<'a>(size: f32, c: Colors) -> Element<'a, Message> {
    let inner = (size * 0.66).round();
    container(iced::widget::image(mark()).width(inner).height(inner))
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
/// The mark and the two-tone wordmark, on the rail. Two plain texts
/// rather than one rich text: the rich text widget resolves heavier faces
/// differently and lands on a monospace fallback for the system sans.
fn brand<'a>(r: Colors) -> Element<'a, Message> {
    row![
        iced::widget::image(mark()).width(26).height(26),
        row![
            heading("Agent", 16).color(r.text),
            heading("Docker", 16).color(super::style::mix(r.accent, iced::Color::WHITE, 0.3))
        ]
    ]
    .spacing(9)
    .align_y(Center)
    .into()
}
/// Nothing here yet, said kindly: the mark in a quiet tile, a title, one
/// sentence and at most one action. No frame: an empty place should not
/// look like a thing to read.
pub(super) fn empty<'a>(
    title_text: &'a str,
    hint: &'a str,
    extra: Option<Element<'a, Message>>,
    c: Colors,
) -> Element<'a, Message> {
    let tile = container(
        iced::widget::image(mark())
            .width(26)
            .height(26)
            .opacity(if c.dark { 0.75_f32 } else { 0.9_f32 }),
    )
    .center(48)
    .style(move |_| c.tile(super::style::RADIUS_LG));
    let mut body = column![
        tile,
        Space::new().height(4),
        heading(title_text, 16),
        note(hint, c).align_x(Center),
    ]
    .spacing(6)
    .align_x(Center);
    if let Some(extra) = extra {
        body = body.push(Space::new().height(8)).push(extra);
    }
    container(body)
        .padding([36, 20])
        .width(Fill)
        .center_x(Fill)
        .into()
}

/// A key as a keycap: monospace on a raised tile with a hairline.
pub(super) fn kbd<'a>(key: impl Into<String>, c: Colors) -> Element<'a, Message> {
    container(
        text(key.into())
            .size(11)
            .line_height(iced::Pixels(14.0))
            .font(Font::MONOSPACE)
            .color(c.muted),
    )
    .padding([1, 5])
    .style(move |_| c.tile(super::style::RADIUS_XS))
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

/// How long since `then`, coarsely: "now", "3m", "1h 12m", "2d". Rows
/// refresh on the clock sweep; seconds would only make them twitch.
pub(super) fn elapsed(now: chrono::DateTime<Utc>, then: chrono::DateTime<Utc>) -> String {
    let minutes = (now - then).num_minutes().max(0);
    match minutes {
        0 => "now".to_owned(),
        m if m < 60 => format!("{m}m"),
        m if m < 60 * 24 => {
            if m % 60 == 0 || m >= 600 {
                format!("{}h", m / 60)
            } else {
                format!("{}h {}m", m / 60, m % 60)
            }
        }
        m => format!("{}d", m / (60 * 24)),
    }
}

/// A card with a header strip: what it holds and why on the left, an
/// optional action on the right, a rule, then the body.
pub(super) fn section<'a>(
    title_text: impl Into<String>,
    description: Option<String>,
    action_element: Option<Element<'a, Message>>,
    body: impl Into<Element<'a, Message>>,
    c: Colors,
) -> Element<'a, Message> {
    let mut words = column![heading(title_text, 15)].spacing(2).width(Fill);
    if let Some(description) = description {
        words = words.push(note(description, c));
    }
    let mut header = row![words].spacing(12).align_y(Center);
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

/// A menu's frame: the overlay surface with a little room inside.
pub(super) fn menu<'a>(items: impl Into<Element<'a, Message>>, c: Colors) -> Element<'a, Message> {
    container(items)
        .padding(4)
        .width(Fill)
        .style(move |_| c.overlay_style())
        .into()
}

/// One entry of a menu; a destructive one is said in red.
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
            // A session started in `/` or the home folder has a project
            // only in name; it is one of the Other sessions.
            None if self.shell.catalog.unassigned => {
                project.is_none_or(|p| self.shell.catalog.broad_unpinned(&p.root))
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
    fn in_project(&self) -> bool {
        !matches!(
            self.screen,
            Screen::Questions | Screen::Runtimes | Screen::Settings | Screen::Desktop
        )
    }
    /// The footer while the daemon is older than this window: what is wrong,
    /// the one thing that fixes it, and what that costs before it is done.
    fn daemon_notice(&self, c: Colors) -> Option<Element<'_, Message>> {
        let version = self.daemon_behind_now()?;
        let connected = self.connected.is_ok();
        let row = if self.daemon_restarting() {
            row![
                dot(c.amber, 7.0, c),
                small("Restarting the background service…", c).width(Fill),
            ]
        } else if self.shell.confirm_daemon_restart {
            let (stop, back) = self.restart_cost();
            let cost = match (stop, back) {
                (0, _) => "Sessions keep running and reconnect on their own.".to_owned(),
                (n, 0) => format!(
                    "{n} session{} AgentDocker started will stop. Sessions started in a terminal keep running.",
                    if n == 1 { "" } else { "s" }
                ),
                (n, m) => format!(
                    "{n} session{} AgentDocker started will stop and {m} will start again. Sessions started in a terminal keep running.",
                    if n == 1 { "" } else { "s" }
                ),
            };
            row![
                dot(c.amber, 7.0, c),
                small(format!("Restart the background service now? {cost}"), c).width(Fill),
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
            row![
                dot(c.amber, 7.0, c),
                small(
                    format!(
                        "The background service is running an older version ({version}) than this app ({}). Restart it to finish updating.",
                        env!("CARGO_PKG_VERSION")
                    ),
                    c
                )
                .width(Fill),
                action(
                    "daemon-restart",
                    "Restart background service…",
                    connected.then_some(Message::ConfirmDaemonRestart(true)),
                    false,
                ),
            ]
        };
        // A strip of the page's own surface, washed amber: it carries
        // the page's controls, which the navy status bar cannot.
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
        let id = agent.id.to_string();
        if self.needs_input(&id) {
            "needs input".to_owned()
        } else if let Some((_, state)) = agentdocker_core::provider_block(agent, &self.agents) {
            state
                .issue
                .as_ref()
                .expect("blocked")
                .kind
                .label()
                .to_owned()
        } else if self.delivery_paused(agent) && agent.status.is_live() {
            "not receiving messages".to_owned()
        } else if self.ended_with_undelivered(agent) {
            format!("ended {}", undelivered_phrase(self.undelivered(agent)))
        } else if agent.status.is_live() {
            match self.activity.get(&id) {
                None | Some(Activity::Unknown) => "running".to_owned(),
                Some(activity) => activity.label().to_owned(),
            }
        } else if let agentdocker_core::AgentStatus::Failed { reason } = &agent.status {
            format!("failed: {reason}")
        } else {
            "ended".to_owned()
        }
    }
    /// The colour that goes with [`Self::activity_label`].
    fn activity_color(&self, agent: &AgentRecord, c: Colors) -> iced::Color {
        if self.needs_input(&agent.id.to_string()) || self.delivery_needs_you(agent) {
            c.amber
        } else if agent.status.is_live() {
            // Green is a report, not a heartbeat: a process we only know is
            // alive has no signal to show green for.
            match self.activity.get(&agent.id.to_string()) {
                None | Some(Activity::Unknown | Activity::Starting) => c.faint,
                Some(_) => c.green,
            }
        } else {
            c.faint
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

    pub(super) fn input_readiness(&self, agent: &AgentRecord) -> &'static str {
        if !agent.status.is_live() {
            return "Session ended";
        }
        if self.connected.is_err() {
            return "Readiness unavailable";
        }
        if let Some((_, state)) = agentdocker_core::provider_block(agent, &self.agents) {
            return state.issue.as_ref().expect("blocked").kind.label();
        }
        let readiness = agentdocker_core::InputReadiness::for_agent(agent, Utc::now());
        // A current receiver with words queued that no receipt covers is
        // waiting on the provider to take them; an earlier receipt does not
        // make them delivered. Limits, pauses and silence come first.
        if matches!(
            readiness,
            agentdocker_core::InputReadiness::Verified
                | agentdocker_core::InputReadiness::AwaitingFirstReceipt
        ) && self
            .awaiting_receipt
            .get(agent.id.as_str())
            .is_some_and(|count| *count > 0)
        {
            return "Sent · waiting for the agent to take it";
        }
        readiness.label()
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
        let window: Element<'_, Message> = column![workspace, self.footer(c)].into();
        // The window is always the first layer of one stack, dialogs or
        // none, so opening one keeps its scroll offsets and field state.
        let mut layers = vec![window];
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
        let mut header_words = column![title(title_text, 20)].spacing(2).width(Fill);
        if in_project && let Some(entry) = self.shell.catalog.selected() {
            header_words = header_words.push(
                text(shorten_home(&entry.project.root))
                    .size(12)
                    .font(Font::MONOSPACE)
                    .color(c.faint)
                    .wrapping(iced::widget::text::Wrapping::None),
            );
        } else if in_project && self.shell.catalog.unassigned {
            header_words = header_words.push(note("Sessions without a known project", c));
        } else if !in_project {
            header_words = header_words.push(note(
                match self.screen {
                    Screen::Questions if self.has_conversations() => {
                        "Channels, direct messages and what your agents were told"
                    }
                    Screen::Questions => "Questions and messages waiting for you",
                    Screen::Runtimes => "Connect and configure your agent tools",
                    _ => "Appearance, terminal, installation and diagnostics",
                },
                c,
            ));
        }
        heading_row = heading_row.push(header_words);
        let mut tools = row![].spacing(8).align_y(Center);
        if in_project && let Some(pause) = self.pause_controls(c) {
            tools = tools.push(pause);
        }
        if in_project && self.shell.catalog.selected().is_some() {
            tools = tools.push(custom(
                "project-terminal",
                "Open project terminal",
                row![
                    icon(Icon::Terminal, c.muted, 14.0),
                    text(if narrow { "Terminal" } else { "Open terminal" })
                        .size(13)
                        .line_height(iced::Pixels(crate::controls::LABEL_LINE))
                        .font(weight(iced::font::Weight::Medium))
                ]
                .spacing(7)
                .align_y(Center),
                (!self.shell.terminal_opening && self.shell.project_available != Some(false))
                    .then_some(Message::OpenProjectTerminal),
                false,
                Kind::Secondary,
                [7, 12],
            ));
        }
        if in_project && let Some(launch) = self.launch_button(c) {
            tools = tools.push(launch);
        }
        let header: Element<'_, Message> = if narrow {
            column![heading_row, tools.wrap()].spacing(12).into()
        } else {
            row![heading_row, tools].spacing(16).align_y(Center).into()
        };
        let mut content = column![header].spacing(16).width(Fill);
        if in_project && let Some(form) = self.pause_form(c) {
            content = content.push(form);
        }
        if let Err(error) = &self.connected {
            content = content.push(attention(
                column![
                    row![
                        dot(c.amber, 8.0, c),
                        heading("Connection unavailable", 15).color(c.amber)
                    ]
                    .spacing(8)
                    .align_y(Center),
                    note(
                        format!("{error}\nShowing the last update. Reconnecting…"),
                        c
                    )
                ]
                .spacing(7),
                c.amber,
                c,
            ));
        }
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
                column![text(error.clone()).size(14).color(c.amber), controls].spacing(8),
                c.amber,
                c,
            ));
        }
        if let Some(error) = &self.shell.error {
            content = content.push(attention(
                column![
                    text(error.clone()).size(14).color(c.amber),
                    ghost("dismiss-error", "Dismiss", Some(Message::DismissError))
                ]
                .spacing(10),
                c.amber,
                c,
            ));
        }
        if !self.status.is_empty() {
            content = content.push(
                row![
                    dot(c.accent, 7.0, c),
                    text(self.status.clone())
                        .size(13)
                        .font(weight(iced::font::Weight::Medium))
                        .color(c.accent_ink)
                ]
                .spacing(8)
                .align_y(Center),
            );
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
                tabs = tabs.push(tab(
                    format!("project-tab-{screen:?}"),
                    label,
                    Some(icon(glyph, if selected { c.text } else { c.muted }, 15.0)),
                    Some(Message::Navigate(screen)),
                    selected,
                ));
            }
            // Underlined only on one of its own screens: with its menu open
            // over Agents, two tabs read as selected at once.
            let more_selected = matches!(
                self.screen,
                Screen::Journal
                    | Screen::Channels
                    | Screen::Leases
                    | Screen::Console
                    | Screen::Usage
            );
            let more_label = match self.screen {
                Screen::Journal => "History",
                Screen::Channels => "Channels",
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
            content = content.push(section(
                "Add an existing project",
                Some("Choose a folder. Nothing is launched or written into it.".to_owned()),
                None,
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
        let body = match self.screen {
            Screen::Chat => self.project_chat_view(c),
            Screen::Agents => self.sessions(c),
            Screen::Board => self.board_view(c),
            Screen::Usage => self.usage_view(c),
            Screen::Questions if self.has_conversations() => self.messages_view(c),
            Screen::Questions => self.questions(c),
            Screen::Runtimes => self.connections(c),
            Screen::Terminal => self.terminal_view(c),
            Screen::Journal => self.journal_view(c),
            Screen::Channels => self.channels_view(c),
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
        // In dark the rail and the page are near in tone, so a hairline
        // parts them; in light the navy rail does that on its own.
        let edge = if c.dark { 1.0 } else { 0.0 };
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

    /// One quiet line across the bottom, part of the navy frame with the
    /// rail: the daemon connection as a dot and a word, and the version.
    /// Said here once, so the rail and the pages need not repeat it.
    fn footer(&self, c: Colors) -> Element<'_, Message> {
        let connected = self.connected.is_ok();
        if let Some(notice) = self.daemon_notice(c) {
            return notice;
        }
        let r = c.rail();
        let (tone, word, detail) = if connected {
            (r.green, "Connected", "— background service")
        } else {
            (r.amber, "Reconnecting", "— background service…")
        };
        container(
            row![
                dot(tone, 7.0, r),
                text(word)
                    .size(12)
                    .font(weight(iced::font::Weight::Medium))
                    .color(tone),
                text(detail).size(12).color(r.muted).width(Fill),
                match self.desktop.update_available() {
                    Some(version) => crate::controls::custom_sized(
                        "open-available-update",
                        format!("Update {version} available"),
                        row![
                            dot(r.accent_ink, 6.0, r),
                            text(format!("Update {version} available"))
                                .size(12)
                                .color(r.accent_ink)
                        ]
                        .spacing(6)
                        .align_y(Center),
                        Some(Message::Navigate(Screen::Desktop)),
                        false,
                        Kind::Nav,
                        [2, 8],
                        iced::Length::Shrink,
                    ),
                    None => text(format!("agentdocker {}", env!("CARGO_PKG_VERSION")))
                        .size(11)
                        .font(Font::MONOSPACE)
                        .color(r.faint)
                        .into(),
                }
            ]
            .spacing(6)
            .align_y(Center),
        )
        .padding([5, 14])
        .width(Fill)
        .style(move |_| container::Style {
            border: iced::Border {
                color: r.line,
                width: 1.0,
                radius: 0.0.into(),
            },
            ..r.surface(r.ground, false)
        })
        .into()
    }

    /// A destination on the rail: glyph, label and an optional count,
    /// amber when it is something waiting on the person.
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
            Some((count, _)) => format!("{label} {count}"),
            None => label.to_owned(),
        };
        if let Some((count, waiting)) = badge {
            content = content.push(count_chip(count, waiting, r));
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
        let mut hint_text = if live > 0 {
            format!("{live} live agent{}", if live == 1 { "" } else { "s" })
        } else if entry.pinned {
            "Pinned · no live agents".to_owned()
        } else {
            "Discovered · no live agents".to_owned()
        };
        if done > 0 {
            hint_text.push_str(&format!(" · {done} finished, not yet viewed"));
        }
        let seed = entry.project.id().to_string();
        // One line each, clipped rather than wrapped or drawn over the
        // marks beside it. Two folders called the same are told apart
        // by where they are, in one short line.
        let mut label = column![
            text(name.clone())
                .size(13.5)
                .wrapping(iced::widget::text::Wrapping::None)
        ]
        .spacing(1)
        .width(Fill);
        if shared.contains(&name) {
            label = label.push(
                text(parent_folder(&path))
                    .size(11.5)
                    .color(r.muted)
                    .wrapping(iced::widget::text::Wrapping::None),
            );
        }
        let label = container(label).width(Fill).clip(true);
        let mut content = row![hint(monogram(&name, &seed, 20.0, r), hint_text, c), label]
            .spacing(6)
            .width(Fill)
            .align_y(Center);
        if done > 0 {
            content = content.push(pill(
                done.to_string(),
                alpha(r.accent, 0.35),
                r.accent_ink,
                r,
            ));
        }
        if live > 0 {
            content = content.push(
                row![
                    dot(r.green, 6.0, r),
                    text(live.to_string()).size(12).color(r.muted)
                ]
                .spacing(5)
                .align_y(Center),
            );
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
            format!("{}{}", if entry.pinned { "• " } else { "" }, name),
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

    /// Live, non-human sessions in the project at `root`.
    fn live_in(&self, root: &std::path::Path) -> usize {
        self.agents
            .iter()
            .filter(|a| {
                a.status.is_live()
                    && a.spec.runtime != agentdocker_core::HUMAN_RUNTIME
                    && a.project.as_ref().is_some_and(|p| p.root == root)
            })
            .count()
    }

    fn sidebar(&self, c: Colors) -> Element<'_, Message> {
        let r = c.rail();
        let project_page = self.in_project();
        let mut nav =
            column![container(brand(r)).padding([0, 8]), Space::new().height(18)].spacing(2);
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
            "inbox",
            if self.has_conversations() {
                "Messages"
            } else {
                "Inbox"
            },
            Icon::Inbox,
            {
                // With conversations the badge is their unread count, the
                // one number the screen itself shows per conversation.
                let now = Utc::now();
                let waiting = if self.has_conversations() {
                    self.unread_total() as usize
                } else {
                    self.questions.iter().filter(|q| !q.expired(now)).count()
                        + self.direct_messages().len()
                };
                (waiting > 0).then_some((waiting, true))
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
        nav = nav
            .push(Space::new().height(18))
            .push(container(eyebrow("Projects", r)).padding([0, 10]))
            .push(Space::new().height(4));
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
            let label = format!("Temporary ({})", temporary.len());
            let mut fold = row![
                text(if open { "▾" } else { "▸" }).size(11).color(r.muted),
                eyebrow(label.clone(), r).width(Fill),
            ]
            .spacing(8)
            .align_y(Center);
            if live > 0 {
                fold = fold.push(
                    row![
                        dot(r.green, 6.0, r),
                        text(live.to_string()).size(12).color(r.muted)
                    ]
                    .spacing(5)
                    .align_y(Center),
                );
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
                .is_none_or(|p| self.shell.catalog.broad_unpinned(&p.root))
                && a.spec.runtime != agentdocker_core::HUMAN_RUNTIME
        }) || self.discovered.iter().any(|p| {
            p.project
                .as_ref()
                .is_none_or(|p| self.shell.catalog.broad_unpinned(&p.root))
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
                    // The rail's own ink for the scroller: the page's
                    // grey would sit on navy like a smudge.
                    .style(move |theme, status| quiet_scroll(theme, status, r))
                    .height(Fill),
            )
            .push(Space::new().height(6))
            .push(container(Space::new().width(Fill).height(1)).style(move |_| r.rule()))
            .push(Space::new().height(6))
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
            items = items.push(
                container(
                    column![
                        input(
                            format!("project-rename-{key}"),
                            "Name",
                            draft,
                            Message::ProjectRenameDraft,
                        ),
                        row![
                            small("Empty goes back to the folder's name.", c).width(Fill),
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
        items = items.push(menu_separator(c)).push(
            container(
                text(shorten_home(&path))
                    .size(11.5)
                    .font(Font::MONOSPACE)
                    .color(c.faint)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
            )
            .padding([4, 8]),
        );
        menu(items, c)
    }

    /// Channel messages still queued for this person, in the projects on view.
    fn queued_channel_messages(&self) -> usize {
        let rooms: BTreeSet<_> = self
            .channels
            .iter()
            .filter(|ch| {
                self.selected_root().is_none()
                    || self
                        .shell
                        .catalog
                        .selected()
                        .is_some_and(|e| e.project.id() == ch.project)
            })
            .map(|ch| ch.id.clone())
            .collect();
        self.inbox
            .iter()
            .filter(|m| match &m.to {
                agentdocker_core::Destination::Channel(ch) => rooms.contains(ch),
                _ => false,
            })
            .count()
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
        let queued = self.queued_channel_messages();
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
            place("project-tab-Journal", "History".to_owned(), Screen::Journal),
            place(
                "project-tab-Channels",
                if queued > 0 {
                    format!("Channels ({queued} waiting)")
                } else {
                    "Channels".to_owned()
                },
                Screen::Channels
            ),
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
            return Some(
                row![
                    row![
                        dot(c.amber, 7.0, c),
                        text(format!("Paused · {}", first_line(&pause.reason, 48)))
                            .size(13)
                            .font(weight(iced::font::Weight::Medium))
                            .color(c.amber),
                    ]
                    .spacing(6)
                    .align_y(Center),
                    action(
                        "resume-project",
                        if sending { "Resuming…" } else { "Resume" },
                        (connected && !sending).then_some(Message::ResumeProject(root.clone())),
                        false
                    ),
                ]
                .spacing(10)
                .align_y(Center)
                .into(),
            );
        }
        if control.and_then(|control| control.draft.as_ref()).is_some() {
            return None;
        }
        Some(custom(
            "pause-project",
            "Pause…",
            row![
                icon(Icon::Pause, c.muted, 12.0),
                text("Pause…")
                    .size(13)
                    .line_height(iced::Pixels(crate::controls::LABEL_LINE))
                    .font(weight(iced::font::Weight::Medium))
            ]
            .spacing(6)
            .align_y(Center),
            (connected && !sending).then_some(Message::PauseStart(root)),
            false,
            Kind::Ghost,
            [7, 10],
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
                "Why: what the agents will read",
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
            form = form
                .push(heading("Pause this project's agents", 14))
                .push(if self.narrow() {
                    Element::from(column![field, buttons].spacing(8))
                } else {
                    row![container(field).width(Fill), buttons]
                        .spacing(10)
                        .align_y(Center)
                        .into()
                });
            form = form.push(small(
                "They finish what they are doing and claim nothing new until you resume.",
                c,
            ));
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
            icon(Icon::ChevronDown, iced::Color::WHITE, 12.0),
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
                        column![
                            text(runtime.label.clone())
                                .size(13)
                                .font(weight(iced::font::Weight::Medium)),
                            text(binary)
                                .size(11)
                                .font(Font::MONOSPACE)
                                .color(c.faint)
                                .wrapping(iced::widget::text::Wrapping::None),
                        ]
                        .spacing(2)
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
            if self.runtimes.iter().all(|r| r.cli.is_none()) {
                items = items
                    .push(container(small("No tool to launch is installed.", c)).padding([6, 8]));
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
            "Find a session…",
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
                column![
                    heading("Project folder unavailable", 16),
                    note("Restore the folder or choose another project.", c),
                    action(
                        "retry-project-folder",
                        "Check folder again",
                        Some(Message::RetryProject),
                        false
                    )
                ]
                .spacing(10),
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
                    Some(
                        text(
                            process
                                .cwd
                                .as_ref()
                                .map(|cwd| shorten_home(cwd))
                                .unwrap_or_else(|| "folder unknown".to_owned()),
                        )
                        .size(12)
                        .font(Font::MONOSPACE)
                        .color(c.faint)
                        .into(),
                    ),
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
            panel_col = panel_col.push(section(
                "Running here, not connected",
                Some(
                    "Started outside AgentDocker. Connect one to see what it is doing and message it."
                        .to_owned(),
                ),
                None,
                rows_list(rows, c),
                c,
            ));
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
                    "No matching sessions",
                    "Try another name, tool, or branch.",
                    Some(
                        column![
                            small(
                                format!("Nothing matches “{}”.", self.shell.search.trim()),
                                c
                            ),
                            action(
                                "session-search-clear",
                                "Clear search",
                                Some(Message::Search(String::new())),
                                false,
                            )
                        ]
                        .spacing(10)
                        .align_x(Center)
                        .into(),
                    ),
                    c,
                ));
            }
            let (title_text, hint) = if !self.shell.search.is_empty() {
                ("", "")
            } else {
                match filter {
                    Filter::Current if self.all_projects() => (
                        "No agents running",
                        "Start Claude Code or Codex in any folder and it appears here, \
                         or choose a project on the left and launch one.",
                    ),
                    Filter::Current => (
                        "No agents in this project",
                        "Press Launch agent, or start Claude Code or Codex in this folder \
                         and it appears here.",
                    ),
                    Filter::NeedsInput => (
                        "Nothing needs your input",
                        "When an agent asks you something, it appears here and in Inbox.",
                    ),
                    Filter::Earlier => ("No sessions", "Nothing has run here yet."),
                }
            };
            if !title_text.is_empty() {
                // An empty list shows the shape of what will fill it.
                panel_col = panel_col
                    .push(column![ghost_rows(c), empty(title_text, hint, None, c)].spacing(0));
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
    /// name over its tool and branch), how it is (a dot and a word, how
    /// long), and the one thing to do with it, only when there is one.
    fn session_rows<'a>(&'a self, records: &[&'a AgentRecord], c: Colors) -> Element<'a, Message> {
        let now = Utc::now();
        let narrow = self.narrow();
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
            let activity = self.activity_label(agent);
            let tone = self.activity_color(agent, c);
            let name = self.display_name(agent);
            // The branch is in the name already when the record has one.
            let branch = agent
                .vcs
                .as_ref()
                .and_then(|v| v.branch.as_deref())
                .filter(|b| !name.contains(b));
            let meta = format!(
                "{}{}",
                branch.map(|b| format!("{b} · ")).unwrap_or_default(),
                activity
            );
            let spoken = format!("{name}\n{} · {}", agent.spec.runtime, meta);
            // A generated name already reads as the tool ("Claude Code ·
            // 0180d761"), so only a chosen name says the tool again.
            let mut facts = Vec::new();
            if !agent.name_is_generated() {
                facts.push(super::runtime_label(&agent.spec.runtime));
            }
            if let Some(branch) = branch {
                facts.push(branch.to_owned());
            }
            let mut words = column![
                text(name.clone())
                    .size(14)
                    .font(weight(iced::font::Weight::Medium))
                    .wrapping(iced::widget::text::Wrapping::None)
            ]
            .spacing(2)
            .width(Fill);
            // Narrow, the status word moves under the name rather than
            // squeezing the name off the row.
            if narrow {
                facts.push(activity.clone());
            }
            if !facts.is_empty() {
                words = words.push(
                    text(facts.join(" · "))
                        .size(12)
                        .color(c.muted)
                        .wrapping(iced::widget::text::Wrapping::None),
                );
            }
            // Time left to answer, as the same small ring Needs you uses.
            let window = self.soonest_answer_window(&id).map(|left| {
                approvals::ring(left, if left < 0.2 { c.red } else { c.amber }, 14.0, c)
            });
            let mut content = row![
                agent_mark(Some(agent.spec.runtime.as_str()), &name, &id, 28.0, c),
                container(words).width(Fill).clip(true)
            ]
            .spacing(12)
            .align_y(Center);
            if self.shell.unviewed_done.contains(&id) {
                // Finished since you last looked. The observed state stays
                // in the status word; this says only that it is new to you.
                content = content.push(pill("Done", c.accent_soft, c.accent_ink, c));
            }
            if let Some(window) = window {
                content = content.push(window);
            }
            if !narrow {
                content = content.push(
                    container(status_word(activity.clone(), tone, c))
                        .width(iced::Length::Shrink)
                        .max_width(220)
                        .clip(true),
                );
                let since = agent.started_at.unwrap_or(agent.created_at);
                content = content.push(
                    container(
                        text(if agent.status.is_live() {
                            elapsed(now, since)
                        } else {
                            elapsed(now, agent.finished_at.unwrap_or(agent.last_seen))
                        })
                        .size(12)
                        .color(c.faint),
                    )
                    .width(44)
                    .align_x(iced::alignment::Horizontal::Right),
                );
            } else {
                content = content.push(dot(tone, 8.0, c));
            }
            // An ended Claude Code session that can come back says so on
            // its row: the one thing to do with it is right there, not
            // behind Details. While its resume is on its way the row says
            // so; a blocker (live, no conversation id, no tool) keeps the
            // row quiet, and Details says why.
            if agent.spec.runtime == "claude-code"
                && !agent.status.is_live()
                && self.reconnect_blocker(agent).is_none()
            {
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
                [if self.settings.roomy { 14 } else { 10 }, 14],
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
            column![
                heading(name.clone(), 15).wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                row![
                    status_word(self.activity_label(agent), self.activity_color(agent, c), c),
                    text(format!("· {}", super::runtime_label(&agent.spec.runtime)))
                        .size(12)
                        .color(c.muted),
                ]
                .spacing(6)
                .align_y(Center)
                .wrap()
            ]
            .spacing(4)
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
        if agent.spec.runtime != "human" {
            let queue = self.queued_inputs.get(&id).map(|count| {
                if *count == 0 {
                    "No queued messages".to_owned()
                } else {
                    format!("{count} queued")
                }
            });
            let mut status = Vec::new();
            if self.connected.is_err() {
                status.push("Last known".to_owned());
            }
            if paused && agent.status.is_live() {
                status.push("Not receiving messages".to_owned());
            } else if agent.status.is_live() {
                status.push(self.input_readiness(agent).to_owned());
            }
            if let Some(queue) = queue {
                status.push(queue);
            }
            if let Some(received_at) = delivery.and_then(|d| d.received_at) {
                status.push(format!(
                    "Last took a message {}",
                    ago(Utc::now(), received_at)
                ));
            }
            if !status.is_empty() {
                body = body.push(small(status.join(" · "), c));
            }
            if let Some((source, state)) = agentdocker_core::provider_block(agent, &self.agents) {
                let issue = state.issue.as_ref().expect("blocked");
                if let Some(reset) = issue.reset_at {
                    body = body.push(small(
                        format!("Provider reset: {} UTC", reset.format("%b %d %H:%M")),
                        c,
                    ));
                }
                body = body.push(small(
                    "Messages are kept. Resume after the provider is available.",
                    c,
                ));
                body = body.push(action(
                    "resume-provider",
                    "Resume delivery",
                    self.connected
                        .is_ok()
                        .then(|| Message::ResumeProvider(source.id.to_string(), state.observed_at)),
                    false,
                ));
            }
            // The daemon gave up starting the bound receiver: the one repair a
            // person can make from here, and the only control that makes one.
            if let Some(binding) = agent.input_binding.as_ref().filter(|b| b.restart.exhausted) {
                body = body.push(small(
                    format!(
                        "Message delivery could not be restarted after {} tries. Its messages are kept.",
                        binding.restart.attempts
                    ),
                    c,
                ));
                body = body.push(action(
                    "retry-receiver",
                    "Restart message delivery",
                    self.connected
                        .is_ok()
                        .then(|| Message::RetryController(agent.id.to_string())),
                    false,
                ));
            }
            // An ended session: its messages are kept in its queue. Say how
            // many, how to get them delivered, and let the person put the
            // notice away. Nothing waiting means nothing to review.
            if paused && !agent.status.is_live() {
                if self.ended_with_undelivered(agent) {
                    body = body.push(
                        text(format!(
                            "This session ended {}.",
                            undelivered_phrase(self.undelivered(agent))
                        ))
                        .size(13)
                        .color(c.amber),
                    );
                    body = body.push(note(
                        "They are kept. Resume the conversation from its project folder and they are delivered to it; nothing is sent anywhere else.",
                        c,
                    ));
                } else {
                    body = body.push(small(
                        "Session ended. Nothing is waiting to be delivered.",
                        c,
                    ));
                }
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
                if self.connected.is_err() {
                    body = body.push(small("Reconnect to the daemon to read the session log.", c));
                }
                if self.shell.review_delivery {
                    if let Some(reason) = delivery.and_then(|d| d.pause_reason.as_deref()) {
                        body = body.push(text(plain_pause_reason(reason)).size(13).color(c.amber));
                    }
                    body = body.push(note("Messages to this session are kept, not lost. Check the log below before restarting it or sending again.", c));
                    if let Some((log_agent, result)) = &self.session_log
                        && log_agent == &id
                    {
                        match result {
                            Err(error) => {
                                body = body.push(
                                    text(format!("Could not read the session log: {error}"))
                                        .size(13)
                                        .color(c.amber),
                                );
                            }
                            Ok(log) if log.is_empty() => {
                                body = body.push(small("The session log is empty.", c));
                            }
                            Ok(log) => {
                                body =
                                    body.push(scrollable(text(log.as_str()).size(12)).height(120));
                            }
                        }
                    } else {
                        body = body.push(small("Loading session log…", c));
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
        let terminal_kind = |label: &'static str, id: &'static str, message: Option<Message>| {
            if question.is_some() || composing {
                action(id, label, message, false)
            } else {
                primary(id, label, message)
            }
        };
        if agent.managed && agent.spec.tty && agent.status.is_live() {
            actions = actions.push(terminal_kind(
                "Open terminal",
                "attach-session",
                self.connected
                    .is_ok()
                    .then_some(Message::Attach(id.clone())),
            ));
        } else if agent.status.is_live() {
            actions = actions.push(terminal_kind(
                "Open original terminal",
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
                    "Message this agent…",
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
                body = body.push(text(error).size(13).color(c.amber));
            } else if entry.is_some_and(|entry| entry.queued.is_some()) {
                let received = entry
                    .and_then(|entry| entry.queued.as_ref())
                    .is_some_and(|id| {
                        delivery
                            .and_then(|d| d.received.as_ref())
                            .is_some_and(|input| input.messages.contains(id))
                    });
                body = body.push(small(
                    if received {
                        "Received by agent"
                    } else {
                        "Message saved to queue"
                    },
                    c,
                ));
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
                                "A name for this session",
                                draft,
                                Message::RenameDraft,
                                true,
                                (self.connected.is_ok() && valid.is_ok())
                                    .then_some(Message::SubmitRename),
                            ),
                            row![
                                action(
                                    "rename-save",
                                    "Save name",
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
                        body = body.push(small(reason, c));
                    }
                }
                _ => {}
            }
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
                        row![
                            icon(Icon::Copy, c.muted, 12.0),
                            text("Copy")
                                .size(12)
                                .font(weight(iced::font::Weight::Medium))
                        ]
                        .spacing(5)
                        .align_y(Center),
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
            facts.push((
                "Folder".to_owned(),
                mono_value(
                    agent
                        .spec
                        .workdir
                        .as_ref()
                        .map(|p| shorten_home(p))
                        .unwrap_or_else(|| "Working folder unknown".into()),
                ),
            ));
            facts.push((
                "Last seen".to_owned(),
                value(ago(Utc::now(), agent.last_seen)),
            ));
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
            if let Some(guidance) =
                super::send_readiness::reconnect(agent, &self.agents, "inspector", c)
            {
                details = details.push(guidance);
                // The relaunch the guidance describes, done here: the
                // session's own tool, its conversation, its folder and the
                // channel, in a pane of this window where Claude's own
                // consent prompt appears. Until its terminal process has
                // ended the button says why it waits.
                if agent.spec.runtime == "claude-code" {
                    let blocker = self.reconnect_blocker(agent);
                    let mut reconnect = row![action(
                        format!("reconnect-{}", agent.id),
                        if self.shell.reconnecting.as_deref() == Some(agent.id.as_str()) {
                            "Reconnecting…"
                        } else {
                            "Reconnect here"
                        },
                        (blocker.is_none() && !self.shell.launching && self.connected.is_ok())
                            .then_some(Message::Reconnect(agent.id.to_string())),
                        false,
                    )]
                    .spacing(8)
                    .align_y(Center);
                    if let Some(why) = blocker {
                        reconnect = reconnect.push(small(why, c));
                    } else {
                        reconnect = reconnect.push(small(
                            "Opens the session in a pane here, with its conversation and live messages; accept Claude's prompt there.",
                            c,
                        ));
                    }
                    details = details.push(reconnect);
                }
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
                    small("Confirm within five seconds to send the stop signal.", c),
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
        let header = column![
            heading("Launch an agent", 16),
            note(
                "Start a tool in this project's folder. Nothing else is changed.",
                c
            )
        ]
        .spacing(4);
        let mut choices = column![].spacing(6);
        for runtime in self.runtimes.iter().filter(|r| r.cli.is_some()) {
            let chosen = self.shell.launch_runtime.as_deref() == Some(runtime.name.as_str());
            let binary = runtime
                .cli
                .as_ref()
                .map(|p| shorten_home(p))
                .unwrap_or_default();
            let initial: String = runtime
                .label
                .chars()
                .next()
                .map(|ch| ch.to_uppercase().collect())
                .unwrap_or_default();
            let radio = container(if chosen {
                Element::from(dot(iced::Color::WHITE, 6.0, c))
            } else {
                Space::new().width(6).height(6).into()
            })
            .center(16)
            .style(move |_| container::Style {
                background: chosen.then(|| c.accent.into()),
                border: iced::Border {
                    color: if chosen { c.accent } else { c.line_strong },
                    width: 1.5,
                    radius: 8.0.into(),
                },
                ..Default::default()
            });
            let content = row![
                match super::logos::Logo::for_runtime(&runtime.name) {
                    Some(logo) => logo_tile(logo, 32.0, c),
                    None => icon_tile(
                        text(initial)
                            .size(13)
                            .font(weight(iced::font::Weight::Semibold))
                            .color(c.muted),
                        32.0,
                        c,
                    ),
                },
                column![
                    text(runtime.label.clone())
                        .size(14)
                        .font(weight(iced::font::Weight::Medium))
                        .color(c.text),
                    text(binary)
                        .size(11.5)
                        .font(Font::MONOSPACE)
                        .color(c.faint)
                        .wrapping(iced::widget::text::Wrapping::None),
                ]
                .spacing(2)
                .width(Fill),
                radio,
            ]
            .spacing(12)
            .align_y(Center);
            choices = choices.push(
                container(custom(
                    format!("launch-tool-{}", runtime.name),
                    runtime.label.clone(),
                    content,
                    (!self.shell.launching).then_some(Message::LaunchRuntime(runtime.name.clone())),
                    false,
                    Kind::Quiet,
                    [10, 12],
                ))
                .style(move |_| container::Style {
                    background: chosen.then(|| alpha(c.accent, 0.06).into()),
                    border: iced::Border {
                        color: if chosen { c.accent } else { c.line },
                        width: 1.0,
                        radius: super::style::RADIUS_MD.into(),
                    },
                    ..Default::default()
                }),
            );
        }
        let mut body = column![
            eyebrow("Tool", c),
            choices,
            Space::new().height(4),
            eyebrow("Name", c),
            input(
                "launch-name",
                "Optional; one is made up otherwise",
                &self.shell.launch_name,
                Message::LaunchName,
            ),
            eyebrow("Arguments", c),
            input(
                "launch-arguments",
                "Command arguments (optional)",
                &self.shell.launch_arguments,
                Message::LaunchArguments,
            ),
        ]
        .spacing(8);
        if matches!(
            self.shell.launch_runtime.as_deref(),
            Some("claude-code" | "codex")
        ) {
            // The normal launch has a receiver. Turning it off is explicit;
            // provider consent remains a separate provider-owned step.
            body = body.push(Space::new().height(4)).push(
                row![
                    column![
                        text("Messages while idle")
                            .size(14)
                            .font(weight(iced::font::Weight::Medium)),
                        small(
                            if !self.shell.launch_channel {
                                "Messages may wait until you interact with this session."
                            } else if self.shell.launch_runtime.as_deref() == Some("codex") {
                                "Opens a Codex conversation here. Messages wait until the current turn finishes."
                            } else {
                                "Connects Claude's experimental channel. Complete Claude's consent in the terminal."
                            },
                            c
                        )
                    ]
                    .spacing(2)
                    .width(Fill),
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
                ]
                .spacing(12)
                .align_y(Center),
            );
        }
        if self.shell.launch_runtime.is_some()
            && !matches!(
                self.shell.launch_runtime.as_deref(),
                Some("claude-code" | "codex")
            )
        {
            body = body.push(small(
                "Automatic idle delivery is not available for this tool.",
                c,
            ));
        }
        if let Some(runtime) = self
            .runtimes
            .iter()
            .find(|r| Some(&r.name) == self.shell.launch_runtime.as_ref())
        {
            body = body.push(
                container(
                    row![
                        text("$").size(12).font(Font::MONOSPACE).color(c.amber),
                        text(format!(
                            "{} {}",
                            runtime
                                .cli
                                .as_ref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_default(),
                            self.shell.launch_arguments
                        ))
                        .size(12)
                        .font(Font::MONOSPACE)
                        .color(c.muted)
                        .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                    ]
                    .spacing(8),
                )
                .padding([8, 10])
                .width(Fill)
                .style(move |_| container::Style {
                    background: Some(if c.dark { c.ground } else { c.hover }.into()),
                    border: iced::Border {
                        color: c.line,
                        width: 1.0,
                        radius: super::style::RADIUS_SM.into(),
                    },
                    ..Default::default()
                }),
            );
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
                (!self.shell.launching
                    && self.shell.launch_runtime.is_some()
                    && self.connected.is_ok())
                .then_some(Message::Launch),
            ),
        ]
        .spacing(8)
        .align_y(Center);
        column![
            container(header).padding([18, 20]),
            rule(c),
            scrollable(container(body).padding([16, 20])).height(iced::Shrink),
            rule(c),
            container(footer).padding([12, 20]),
        ]
        .into()
    }

    /// Messages sent to the person directly, oldest first, minus the ones
    /// that are questions (those have their own cards).
    fn direct_messages(&self) -> Vec<&agentdocker_core::Envelope> {
        let (_, mut direct) = by_room(&self.inbox);
        direct.retain(|message| !self.questions.iter().any(|q| q.id == message.id));
        direct
    }

    /// The least time left among this agent's unanswered questions, as a
    /// fraction of its window; `None` when nothing is waiting.
    fn soonest_answer_window(&self, agent: &str) -> Option<f32> {
        let now = Utc::now();
        self.questions
            .iter()
            .filter(|q| q.from == agent && !q.expired(now))
            .map(|q| remaining_fraction(q.asked_at, q.expires_at, now))
            .min_by(|a, b| a.total_cmp(b))
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

    fn channels_view(&self, c: Colors) -> Element<'_, Message> {
        let selected = self.shell.catalog.selected().map(|e| e.project.id());
        let (messages, _) = by_room(self.inbox.iter().chain(&self.sent_channels));
        // With no project chosen (All projects) every channel is shown;
        // "Reviews" from a conversation lands here from anywhere and must not
        // find "No channels yet" about the one just open. Channels agents
        // opened come first; the rooms AgentDocker opens when checkouts
        // overlap sit folded behind Overlaps (n), as Messages folds them.
        let on_view: Vec<_> = self
            .channels
            .iter()
            .filter(|ch| {
                selected
                    .as_ref()
                    .is_none_or(|project| &ch.project == project)
            })
            .collect();
        let is_overlap = |ch: &&agentdocker_core::Channel| {
            matches!(
                ch.subject,
                agentdocker_core::ChannelSubject::Contested { .. }
            )
        };
        let overlaps = on_view.iter().filter(|ch| is_overlap(ch)).count();
        let named: Vec<_> = on_view
            .iter()
            .filter(|ch| !is_overlap(ch))
            .copied()
            .collect();
        let folded: Vec<_> = on_view
            .iter()
            .filter(|ch| is_overlap(ch))
            .copied()
            .collect();
        let count = on_view.len();
        if count == 0 {
            return empty(
                "No channels yet",
                "Agents open channels here when they coordinate on a task.",
                None,
                c,
            );
        }
        let mut list = column![
            row![
                eyebrow(format!("Channels · {count}"), c).width(Fill),
                small(
                    "Messages for you and sends from this window · partial history",
                    c
                )
                .color(c.faint)
            ]
            .spacing(12)
            .align_y(Center)
        ]
        .spacing(12);
        // Open, the fold sits where the overlap rooms begin.
        let fold_at = (overlaps > 0 && self.shell.overlaps_open).then_some(named.len());
        let mut shown = named;
        if self.shell.overlaps_open {
            shown.extend(folded);
        }
        for (index, channel) in shown.into_iter().enumerate() {
            if fold_at == Some(index) {
                list = list.push(self.overlaps_toggle(overlaps, c));
            }
            let id = channel.id.to_string();
            let body = self.channel_body(channel, messages.get(&id), c);
            let card = container(column![
                container(self.channel_header(channel, c)).padding([12, 16]),
                rule(c),
                container(body).padding([12, 16]).width(Fill),
            ])
            .width(Fill)
            .style(move |_| c.card_style());
            list = list.push(container(card).id(format!("notification-channel-{id}")));
        }
        // Folded, the overlap rooms are not in the loop: the fold that
        // opens them follows the named channels.
        if overlaps > 0 && !self.shell.overlaps_open {
            list = list.push(self.overlaps_toggle(overlaps, c));
        }
        list.into()
    }

    /// A channel's header: what kind of room on a tile, its name with
    /// whether it is open as a dot and a word, what it is for and who is
    /// in it on one quiet line, and their faces at the right.
    fn channel_header(
        &self,
        channel: &agentdocker_core::Channel,
        c: Colors,
    ) -> Element<'_, Message> {
        let open = channel.is_open();
        let (title_text, purpose) = channel_heading(channel);
        let glyph = if channel.paths().is_empty() {
            Icon::Hash
        } else {
            Icon::File
        };
        let mut about = purpose.map_or_else(Vec::new, |p| vec![p]);
        about.push(self.members_line(&channel.members));
        row![
            icon_tile(
                icon(glyph, if open { c.muted } else { c.faint }, 14.0),
                28.0,
                c
            ),
            column![
                row![
                    text(title_text)
                        .size(14)
                        .font(weight(iced::font::Weight::Semibold))
                        .color(if open { c.text } else { c.muted }),
                    if open {
                        status_word("Open", c.green, c)
                    } else {
                        status_word("Closed", c.faint, c)
                    },
                ]
                .spacing(10)
                .align_y(Center),
                small(about.join(" · "), c).wrapping(iced::widget::text::Wrapping::Word),
            ]
            .spacing(3)
            .width(Fill),
            self.facepile(&channel.members, c),
        ]
        .spacing(12)
        .align_y(Center)
        .into()
    }

    /// The first members' faces, overlapping, each ringed in the card's
    /// colour, and how many more there are.
    fn facepile<'a>(
        &self,
        members: &[agentdocker_core::AgentId],
        c: Colors,
    ) -> Element<'a, Message> {
        const FACE: f32 = 24.0;
        const STEP: f32 = 17.0;
        const SHOWN: usize = 4;
        let ring = move |fill: iced::Color| container::Style {
            background: Some(fill.into()),
            border: iced::Border {
                color: c.card,
                width: 2.0,
                radius: 999.0.into(),
            },
            ..Default::default()
        };
        let rest = members.len().saturating_sub(SHOWN);
        let discs = members.len().min(SHOWN) + usize::from(rest > 0);
        let width = if discs == 0 {
            0.0
        } else {
            FACE + STEP * (discs - 1) as f32
        };
        let mut pile = iced::widget::Stack::new().push(Space::new().width(width).height(FACE));
        for (index, id) in members.iter().take(SHOWN).enumerate() {
            let name = self.name_of(id.as_str());
            // A member's disc carries its tool's logo where there is one,
            // its initial on its own tint otherwise.
            let face: Element<'a, Message> = match self
                .runtime_of(id.as_str())
                .and_then(super::logos::Logo::for_runtime)
            {
                Some(logo) => container(
                    iced::widget::image(logo.handle(c.dark))
                        .width(14)
                        .height(14),
                )
                .center(FACE)
                .style(move |_| ring(c.raised))
                .into(),
                None => {
                    let (tint, ink) = super::style::identity(id.as_str(), c.dark);
                    let initial: String = name
                        .chars()
                        .find(|ch| ch.is_alphanumeric())
                        .map(|ch| ch.to_uppercase().collect())
                        .unwrap_or_else(|| "·".to_owned());
                    container(
                        text(initial)
                            .size(11)
                            .font(weight(iced::font::Weight::Semibold))
                            .color(ink),
                    )
                    .center(FACE)
                    .style(move |_| ring(tint))
                    .into()
                }
            };
            pile = pile.push(container(face).padding(iced::Padding {
                left: STEP * index as f32,
                ..iced::Padding::ZERO
            }));
        }
        if rest > 0 {
            pile = pile.push(
                container(
                    container(
                        text(format!("+{rest}"))
                            .size(10)
                            .font(weight(iced::font::Weight::Medium))
                            .color(c.muted),
                    )
                    .center(FACE)
                    .style(move |_| ring(c.raised)),
                )
                .padding(iced::Padding {
                    left: STEP * SHOWN as f32,
                    ..iced::Padding::ZERO
                }),
            );
        }
        pile.into()
    }

    /// Under a channel's header: its reviews and how it closed, what is
    /// queued in it, and the way to write to it.
    fn channel_body<'a>(
        &'a self,
        channel: &'a agentdocker_core::Channel,
        queued: Option<&Vec<&'a agentdocker_core::Envelope>>,
        c: Colors,
    ) -> Element<'a, Message> {
        let id = channel.id.to_string();
        let open = channel.is_open();
        let mut body = column![].spacing(12).width(Fill);
        if !channel.reviews.is_empty() {
            let mut reviews = column![].spacing(6);
            for review in &channel.reviews {
                let (word, tone) = match review.verdict {
                    agentdocker_core::channel::Verdict::Approve => ("Approved", c.green),
                    agentdocker_core::channel::Verdict::Changes => ("Changes asked for", c.amber),
                    agentdocker_core::channel::Verdict::Comment => ("Comment", c.muted),
                };
                reviews = reviews.push(
                    row![
                        status_word(word, tone, c),
                        text(format!(
                            "{} on {}'s work: {}",
                            review.by_name, review.of_name, review.note
                        ))
                        .size(13)
                        .wrapping(iced::widget::text::Wrapping::Word)
                        .width(Fill),
                    ]
                    .spacing(10)
                    .align_y(Center),
                );
            }
            body = body.push(reviews);
        }
        if let Some(resolution) = &channel.resolution {
            body = body.push(note(resolution.clone(), c));
        }
        match queued {
            Some(queued) if !queued.is_empty() => {
                body = body.push(self.transcript(self.recent_window(queued, 20).into_iter(), c));
            }
            _ => body = body.push(note("No messages to show in this channel yet.", c)),
        }
        let writing = self.shell.channel_target.as_deref() == Some(id.as_str());
        body = body.push(action(
            format!("reply-channel-{id}"),
            "Write to channel",
            // A closed channel takes no messages: the control would do
            // nothing, so it is not offered as if it would.
            (self.connected.is_ok() && open).then_some(Message::ChannelTarget(id.clone())),
            writing,
        ));
        if writing {
            let draft = self
                .shell
                .channel_drafts
                .get(&id)
                .cloned()
                .unwrap_or_default();
            if let Some(error) = &draft.error {
                body = body.push(text(error.clone()).size(13).color(c.amber));
            }
            if let Some(notice) = super::send_readiness::notice(
                &draft,
                super::shell::DeliveryTarget::Channel(id.clone()),
                c,
            ) {
                body = body.push(notice);
            }
            let ready =
                draft.sending.is_none() && self.connected.is_ok() && !draft.text.trim().is_empty();
            body = body.push(
                row![
                    composer(
                        "channel-message",
                        id.clone(),
                        "Message",
                        &draft.text,
                        Message::ChannelDraft,
                        draft.sending.is_none(),
                        ready.then_some(Message::SendChannel),
                    ),
                    primary(
                        "send-channel",
                        if draft.sending.is_some() {
                            "Sending…"
                        } else {
                            "Send message"
                        },
                        ready.then_some(Message::SendChannel),
                    )
                ]
                .spacing(8)
                .align_y(Center),
            );
        }
        body.into()
    }

    /// The fold for AgentDocker's overlap rooms on the Channels screen.
    fn overlaps_toggle(&self, count: usize, c: Colors) -> Element<'_, Message> {
        let open = self.shell.overlaps_open;
        row![
            custom(
                "channels-overlaps",
                format!("{} Overlaps ({count})", if open { "▾" } else { "▸" }),
                row![
                    icon(
                        if open {
                            Icon::ChevronDown
                        } else {
                            Icon::ChevronRight
                        },
                        c.muted,
                        13.0
                    ),
                    text(format!("Overlaps ({count})"))
                        .size(13)
                        .line_height(iced::Pixels(crate::controls::LABEL_LINE))
                        .font(weight(iced::font::Weight::Medium)),
                ]
                .spacing(6)
                .align_y(Center),
                Some(Message::ToggleOverlaps),
                false,
                Kind::Ghost,
                [7, 8],
            ),
            small(
                "Rooms AgentDocker opens when two checkouts change the same files",
                c
            )
            .color(c.faint),
        ]
        .spacing(8)
        .align_y(Center)
        .into()
    }

    /// Who is in a channel, in one line: the count and at most four names.
    fn members_line(&self, members: &[agentdocker_core::AgentId]) -> String {
        const NAMED: usize = 4;
        let names: Vec<String> = members
            .iter()
            .take(NAMED)
            .map(|id| self.name_of(id.as_str()))
            .collect();
        let rest = members.len().saturating_sub(NAMED);
        let who = if rest > 0 {
            format!("{} and {rest} more", names.join(", "))
        } else {
            names.join(", ")
        };
        format!(
            "{} member{} · {who}",
            members.len(),
            if members.len() == 1 { "" } else { "s" }
        )
    }

    /// History: one row per journal entry, newest first, in one card —
    /// what kind of thing happened on a tile, the line itself, what kind
    /// and where under it, and how long ago at the right.
    fn journal_view(&self, c: Colors) -> Element<'_, Message> {
        if self.journal.is_empty() {
            return empty(
                "No activity recorded yet",
                "Commits, arrivals, finished work and notes from this project appear here.",
                None,
                c,
            );
        }
        let mut rows = column![].width(Fill);
        for (index, entry) in self.journal.iter().rev().enumerate() {
            if index > 0 {
                rows = rows.push(rule(c));
            }
            rows = rows.push(self.history_row(entry, c));
        }
        container(column![
            pane_header(
                "Activity",
                format!(
                    "{} of the latest {JOURNAL_WINDOW} · earlier entries stay in the journal",
                    self.journal.len()
                ),
                c
            ),
            rows
        ])
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
        let intro = note(
            "What each agent has said it is working on. Others wait, or ask, before touching the same thing.",
            c,
        );
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
            let mut line = row![
                column![
                    text(resource_label(
                        &lease.resource.to_string(),
                        self.selected_root()
                    ))
                    .size(12)
                    .font(Font::MONOSPACE)
                    .color(c.text)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                    small(about.join(" · "), c).wrapping(iced::widget::text::Wrapping::Word),
                ]
                .spacing(4)
                .width(Fill)
            ]
            .spacing(12)
            .align_y(Center);
            if low {
                line = line.push(status_word(
                    if left_secs > 0 { "Expiring" } else { "Expired" },
                    c.amber,
                    c,
                ));
            }
            line = line
                .push(
                    container(meter(
                        left,
                        if low {
                            c.amber
                        } else {
                            super::style::mix(c.faint, c.text, 0.2)
                        },
                        c,
                    ))
                    .width(64),
                )
                .push(
                    container(
                        text(if left_secs > 0 {
                            format!("{} left", super::span(left_secs))
                        } else {
                            "expired".to_owned()
                        })
                        .size(12)
                        .color(if low { c.amber } else { c.faint }),
                    )
                    .width(56)
                    .align_x(iced::alignment::Horizontal::Right),
                );
            rows = rows.push(container(line).padding([10, 16]));
        }
        if count == 0 {
            return column![
                intro,
                empty(
                    "Nothing in use",
                    "When an agent claims a file or resource in this project it appears here.",
                    None,
                    c,
                )
            ]
            .spacing(12)
            .into();
        }
        column![
            intro,
            container(column![
                pane_header("Files in use", format!("{count} held"), c),
                rows
            ])
            .width(Fill)
            .style(move |_| c.card_style())
        ]
        .spacing(12)
        .into()
    }
}

/// A channel's heading and what it is about. A named room is its
/// `#name` over its purpose; an overlap room is "Contested paths (338)"
/// over "view.rs, shell.rs and 336 more" in one bounded line, never the
/// list itself as a title.
fn channel_heading(channel: &agentdocker_core::Channel) -> (String, Option<String>) {
    match &channel.subject {
        agentdocker_core::ChannelSubject::Contested { paths } if !paths.is_empty() => {
            const NAMED: usize = 3;
            let named: Vec<String> = paths
                .iter()
                .take(NAMED)
                .map(|p| p.display().to_string())
                .collect();
            let rest = paths.len().saturating_sub(NAMED);
            let detail = if rest > 0 {
                format!("{} and {rest} more", named.join(", "))
            } else {
                named.join(", ")
            };
            (format!("Contested paths ({})", paths.len()), Some(detail))
        }
        _ => match &channel.name {
            Some(name) if !name.is_empty() => (format!("#{name}"), Some(channel.title())),
            _ => (channel.title(), None),
        },
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
    // clipped line still carries it: `agentdocker-delivery · /private/tmp`,
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
                "/private/tmp/agentdocker-delivery/workspace"
            )),
            "agentdocker-delivery · /private/tmp"
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
            app.input_readiness(&agent),
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
            app.input_readiness(&agent),
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
        });
        assert_eq!(app.input_readiness(&agent), "Ready for messages");
        // Words queued behind a current receiver that no receipt covers are
        // waiting on the provider; an earlier receipt does not make them
        // delivered, and a receipt for them does, acknowledged or not.
        app.queued_inputs.insert(agent.id.to_string(), 2);
        app.awaiting_receipt.insert(agent.id.to_string(), 2);
        assert_eq!(
            app.input_readiness(&agent),
            "Sent · waiting for the agent to take it"
        );
        agent.input_delivery.as_mut().unwrap().received = Some(agentdocker_core::ReceivedInput {
            messages: vec!["m1".to_owned().into()],
            receipt: agentdocker_core::InputReceipt::ClaudeChannel,
        });
        agent.input_delivery.as_mut().unwrap().received_at = Some(now);
        app.awaiting_receipt.insert(agent.id.to_string(), 1);
        assert_eq!(
            app.input_readiness(&agent),
            "Sent · waiting for the agent to take it"
        );
        app.awaiting_receipt.insert(agent.id.to_string(), 0);
        assert_eq!(app.input_readiness(&agent), "Receiving messages");
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
        assert_eq!(app.input_readiness(&agent), "Not heard from recently");
        agent.process_started_at = Some(birth);
        agent.input_delivery.as_mut().unwrap().paused = true;
        assert_eq!(app.input_readiness(&agent), "Not receiving messages");
        app.connected = Err("offline".into());
        assert!(!app.tool_reports("codex"));
        assert_eq!(app.input_readiness(&agent), "Readiness unavailable");
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
