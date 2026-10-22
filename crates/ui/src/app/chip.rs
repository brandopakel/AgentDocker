//! A state, said in words: a small pill with an icon and the state's word
//! in its tone. Every state the app shows is one, the healthy ones too — a
//! session working, a message delivered, a tool receiving — so nothing has
//! to be learnt from a colour or found on the pointer. The words are the
//! chip's accessible name; a tooltip, when there is one, only adds detail
//! (a time, a reason, who), and the same detail is the accessible value.
//! Where a state has a fix, the fix's button sits beside the chip, never
//! inside it.
//!
//! The words come from each state's own word function — the standing's
//! label, the readiness labels, the delivery words, lane and plan labels —
//! so a state reads the same on every screen. A chip is one line of a
//! fixed height that fits a row; the row's name is cut before the chip is.
use super::Message;
use super::icons::{Icon, icon};
pub(super) use super::style::Tone;
use super::style::{Colors, weight};
use crate::accessibility::Semantic;
use crate::controls::Control;
use agentdocker_core::standing::Standing;
use iced::{
    Center, Element,
    widget::{container, row, text},
};

/// Every chip is this tall, whatever it says: it sits in a row of
/// 13.5-point names without making the row taller.
pub(super) const HEIGHT: f32 = 18.0;

/// One state as its chip says it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Chip {
    /// The accessible node's id: unique on its screen.
    pub id: String,
    pub icon: Option<Icon>,
    /// What the chip says, and its accessible name.
    pub words: String,
    pub tone: Tone,
    /// More than the words say, on the pointer and as the accessible
    /// value. Never the meaning: the words carry that.
    pub detail: Option<String>,
}

impl Chip {
    pub fn new(
        id: impl Into<String>,
        icon: Option<Icon>,
        words: impl Into<String>,
        tone: Tone,
    ) -> Self {
        Self {
            id: id.into(),
            icon,
            words: words.into(),
            tone,
            detail: None,
        }
    }

    /// Adds detail. Detail that only repeats the words is dropped, so a
    /// state whose full word is its label carries no tooltip.
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        let repeats =
            detail.trim().is_empty() || detail.trim().eq_ignore_ascii_case(self.words.trim());
        self.detail = (!repeats).then_some(detail);
        self
    }

    /// The pill alone, with no accessible node of its own: the face of a
    /// control that is itself a state (the pull request chip).
    pub fn look<'a>(&self, c: Colors) -> Element<'a, Message> {
        let (wash, ink) = c.chip(self.tone);
        let mut content = row![].spacing(4).align_y(Center);
        if let Some(glyph) = self.icon {
            content = content.push(icon(glyph, ink, 10.0));
        }
        content = content.push(
            text(self.words.clone())
                .size(11)
                .line_height(iced::Pixels(14.0))
                .font(weight(iced::font::Weight::Medium))
                .color(ink)
                .wrapping(iced::widget::text::Wrapping::None),
        );
        container(content)
            .padding([0, 7])
            .height(HEIGHT)
            .align_y(iced::alignment::Vertical::Center)
            .style(move |_| c.pill(wash, ink))
            .into()
    }

    /// The chip: its look, named by its words, its detail on the pointer.
    pub fn view<'a>(self, c: Colors) -> Element<'a, Message> {
        let look = self.look(c);
        let control: Element<'a, Message> = Control {
            content: look,
            semantic: Semantic {
                role: accesskit::Role::Label,
                value: self.detail.clone(),
                ..Semantic::button(self.id, self.words, None)
            },
            button: false,
        }
        .into();
        match self.detail {
            Some(detail) => super::view::tip(control, detail, c),
            None => control,
        }
    }
}

/// A chip with no detail, drawn.
pub(super) fn chip<'a>(
    id: impl Into<String>,
    icon: Option<Icon>,
    words: impl Into<String>,
    tone: Tone,
    c: Colors,
) -> Element<'a, Message> {
    Chip::new(id, icon, words, tone).view(c)
}

/// Words as a chip says them: the first letter a capital.
pub(super) fn sentence(words: &str) -> String {
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The head of some words, for a chip: their first line up to the first
/// sentence or clause break, without a closing full stop, cut at `limit`
/// characters. The words whole go in the chip's detail.
pub(super) fn headline(words: &str, limit: usize) -> String {
    let line = super::view::first_line(words, usize::MAX);
    let head = [". ", ": ", "; "]
        .iter()
        .filter_map(|mark| line.find(mark))
        .min()
        .map_or(line.as_str(), |at| &line[..at]);
    super::view::first_line(head.trim_end_matches('.'), limit)
}

/// Where an agent stands, the same on every row that shows it: its label,
/// and its full word as the detail where the label says less.
pub(super) fn standing(id: impl Into<String>, standing: Standing) -> Chip {
    let (glyph, tone) = match standing {
        Standing::NeedsYou(_) => (Some(Icon::Alert), Tone::Attention),
        Standing::Failed => (Some(Icon::Close), Tone::Alert),
        Standing::Working => (Some(Icon::Pulse), Tone::Neutral),
        Standing::Parked => (Some(Icon::Hourglass), Tone::Neutral),
        Standing::Starting => (Some(Icon::Ring), Tone::Neutral),
        Standing::FinishedUnseen => (Some(Icon::Check), Tone::Info),
        // Running claims nothing it has no report of: not working, not
        // idle.
        Standing::Running | Standing::Idle | Standing::Ended => (None, Tone::Neutral),
    };
    Chip::new(id, glyph, standing.label(), tone).detail(sentence(standing.word()))
}

/// How a message stands with one recipient, toned by how much it asks of
/// the person: a problem amber, a wait quiet, a delivery green, an answer
/// the accent.
pub(super) fn delivery_tone(state: agentdocker_core::delivery::State) -> Tone {
    use agentdocker_core::delivery::State as D;
    match state {
        D::Answered => Tone::Info,
        D::Delivered | D::Received | D::Shown => Tone::Positive,
        D::Paused | D::Blocked | D::Silent | D::Ended => Tone::Attention,
        D::Unreachable
        | D::WaitingForPrompt
        | D::WaitsForCheckIn
        | D::WaitingToTake
        | D::Gone
        | D::Unknown => Tone::Neutral,
    }
}

fn delivery_icon(state: agentdocker_core::delivery::State) -> Option<Icon> {
    use agentdocker_core::delivery::State as D;
    match state {
        D::Answered => Some(Icon::Channels),
        D::Delivered | D::Received | D::Shown => Some(Icon::Check),
        D::WaitingForPrompt | D::WaitsForCheckIn | D::WaitingToTake => Some(Icon::Clock),
        D::Paused => Some(Icon::Pause),
        D::Blocked => Some(Icon::Hourglass),
        D::Silent | D::Unreachable => Some(Icon::Alert),
        D::Ended | D::Gone => Some(Icon::Close),
        D::Unknown => None,
    }
}

/// A message's delivery as one chip: the loudest state's word — a problem
/// before a wait, a wait before a delivery, a delivery before an answer —
/// and, when it went to several, how many that word is true of: *Delivered
/// · 2* when it is all of them, *Waiting · 1 of 3* when it is some. `None`
/// for a message that went to nobody.
pub(super) fn delivery(
    id: impl Into<String>,
    states: &[agentdocker_core::delivery::State],
) -> Option<Chip> {
    let rank = |state: agentdocker_core::delivery::State| match delivery_tone(state) {
        Tone::Attention | Tone::Alert => 0,
        Tone::Neutral => 1,
        Tone::Positive => 2,
        Tone::Info => 3,
    };
    let lead = states.iter().copied().min_by_key(|state| rank(*state))?;
    let word = lead.word();
    let alike = states.iter().filter(|state| state.word() == word).count();
    let words = match states.len() {
        1 => word.to_owned(),
        all if alike == all => format!("{word} · {all}"),
        all => format!("{word} · {alike} of {all}"),
    };
    Some(Chip::new(
        id,
        delivery_icon(lead),
        words,
        delivery_tone(lead),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::view::testing::seen;
    use agentdocker_core::delivery::State as D;
    use agentdocker_core::standing::Need;

    /// A chip says its words, is named by them, keeps its detail for the
    /// pointer and the accessible value, and drops detail that repeats it.
    #[test]
    fn a_chip_is_its_words_with_detail_only_for_more() {
        let c = Colors::new(false);
        let view = seen(
            Chip::new("state", Some(Icon::Check), "Delivered · 2", Tone::Positive)
                .detail("agent-a · delivered\nagent-b · delivered")
                .view(c),
        );
        assert!(view.texts.iter().any(|t| t == "Delivered · 2"));
        let node = view.control("state").expect("a named node");
        assert_eq!(node.1, "Delivered · 2", "the accessible name is the words");
        assert_eq!(
            node.2.as_deref(),
            Some("agent-a · delivered\nagent-b · delivered"),
            "the detail is the value"
        );
        assert!(!node.3, "a state, not a control to press");
        let plain = Chip::new("idle", None, "Idle", Tone::Neutral).detail("idle");
        assert_eq!(
            plain.detail, None,
            "detail that repeats the words is dropped"
        );
        let view = seen(plain.view(c));
        assert_eq!(view.control("idle").unwrap().2, None);
        assert_eq!(
            sentence("finished since you looked"),
            "Finished since you looked"
        );
        assert_eq!(sentence(""), "");
        assert_eq!(
            headline(
                "This diff is too large to show here; the file list is complete.",
                60
            ),
            "This diff is too large to show here"
        );
        assert_eq!(
            headline("Inventory incomplete: no permission\nmore", 60),
            "Inventory incomplete"
        );
        assert_eq!(
            headline("Couldn’t check for updates.", 60),
            "Couldn’t check for updates"
        );
        assert_eq!(headline("abcdef", 3), "abc…");
    }

    /// The chip is one fixed-height line, whatever it says, so a row with
    /// one is no taller than a row without.
    #[test]
    fn a_chip_keeps_its_height() {
        use iced::advanced::layout;
        use iced::advanced::widget::Tree;
        let renderer = iced::Renderer::new(iced::Font::DEFAULT, 14.0.into());
        for (glyph, words) in [
            (None, "Idle"),
            (Some(Icon::Alert), "Needs you · 12"),
            (Some(Icon::Clock), "Messages may wait for its next prompt"),
        ] {
            let mut element = Chip::new("h", glyph, words, Tone::Attention).view(Colors::new(true));
            let mut tree = Tree::new(&element);
            let node = element.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(iced::Size::ZERO, iced::Size::new(900.0, 600.0)),
            );
            assert_eq!(node.size().height, HEIGHT, "{words}");
        }
    }

    /// Every standing is a chip in the same words and tone wherever an
    /// agent is shown; the label is short, the full word its detail.
    #[test]
    fn a_standing_chip_says_its_label() {
        let needs = standing("s", Standing::NeedsYou(Need::Delivery));
        assert_eq!(needs.words, "Needs you");
        assert_eq!(needs.tone, Tone::Attention);
        assert_eq!(needs.detail.as_deref(), Some("Not receiving messages"));
        let finished = standing("s", Standing::FinishedUnseen);
        assert_eq!(
            (finished.words.as_str(), finished.tone),
            ("Finished", Tone::Info)
        );
        assert_eq!(
            finished.detail.as_deref(),
            Some("Finished since you looked")
        );
        let idle = standing("s", Standing::Idle);
        assert_eq!((idle.words.as_str(), idle.detail), ("Idle", None));
        let running = standing("s", Standing::Running);
        assert_eq!(
            (running.words.as_str(), running.tone),
            ("Running", Tone::Neutral),
            "a live session nothing reports on does not claim to be idle"
        );
        assert_eq!(standing("s", Standing::Failed).tone, Tone::Alert);
        assert_eq!(standing("s", Standing::Working).words, "Working");
    }

    /// A message's chip leads with the loudest state and counts how many
    /// recipients it is true of.
    #[test]
    fn a_delivery_chip_says_the_loudest_state_and_how_many() {
        let words = |states: &[D]| delivery("d", states).map(|c| (c.words, c.tone));
        assert_eq!(words(&[]), None);
        assert_eq!(
            words(&[D::Delivered]),
            Some(("Delivered".into(), Tone::Positive))
        );
        assert_eq!(
            words(&[D::Delivered, D::Received]),
            Some(("Delivered · 1 of 2".into(), Tone::Positive)),
            "received is its own word"
        );
        assert_eq!(
            words(&[D::Delivered, D::Delivered]),
            Some(("Delivered · 2".into(), Tone::Positive))
        );
        assert_eq!(
            words(&[D::Answered, D::WaitingForPrompt, D::Delivered]),
            Some(("Waiting · 1 of 3".into(), Tone::Neutral)),
            "a wait before a delivery before an answer"
        );
        assert_eq!(
            words(&[D::WaitingToTake, D::WaitsForCheckIn, D::Paused]),
            Some(("Paused · 1 of 3".into(), Tone::Attention)),
            "a problem first"
        );
        assert_eq!(
            words(&[D::WaitingToTake, D::WaitsForCheckIn]),
            Some(("Waiting · 2".into(), Tone::Neutral)),
            "two reasons to wait are one word"
        );
        assert_eq!(
            words(&[D::Ended, D::Gone]),
            Some(("Not delivered · 2".into(), Tone::Attention))
        );
        assert_eq!(words(&[D::Answered]), Some(("Answered".into(), Tone::Info)));
    }
}
