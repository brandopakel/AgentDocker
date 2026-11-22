//! What a verdict keeps of its reviewer's model, beside the runtime it ran
//! in: what was asked for at launch (configured), and which providers and
//! models answered the reviewer's own session in an observation window
//! (observed), as the retained usage data showed when the verdict was
//! given. Kept once, with the verdict, and never rewritten.
//!
//! Neither proves that every step of the review used that model. Observed
//! evidence is what retained data showed at capture, not that collection
//! was on through the window; an inferred window (from the reviewer's
//! previous verdict on the branch, or its record's start) is said as
//! inferred, never as "the model used for this review".

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Whether a verdict's finding counts were stated by its reviewer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingsPresence {
    /// The reviewer gave its counts.
    Stated,
    /// The reviewer gave none: the counts say nothing.
    NotStated,
    /// Given by a client or daemon from before this was kept: the counts
    /// are as recorded, and whether they were stated cannot be told.
    #[default]
    Unknown,
}

impl FindingsPresence {
    pub fn word(self) -> &'static str {
        match self {
            FindingsPresence::Stated => "stated",
            FindingsPresence::NotStated => "not_stated",
            FindingsPresence::Unknown => "unknown",
        }
    }
}

/// The reviewer's model evidence, as kept on its verdict.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEvidence {
    /// What the reviewer's record asked for at launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configured: Option<Configured>,
    /// The provider's session id on the reviewer's record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<Observed>,
}

/// A provider and model as recorded, either absent when not recorded.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Configured {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Where an observation window's start came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowSource {
    /// The reviewer said it (`verdict --since`): its assertion, not proof.
    Stated,
    /// Inferred: the reviewer's previous verdict on this branch.
    PreviousVerdict,
    /// Inferred: the reviewer's record's start.
    RecordStart,
}

/// The samples read: those whose own time is in `[from, to]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub source: WindowSource,
}

/// The window for a verdict given at `to`: the reviewer's stated start,
/// else its previous verdict on the branch, else its record's start;
/// `None` when none is known. A stated start after the verdict is refused.
pub fn window(
    stated: Option<DateTime<Utc>>,
    previous: Option<DateTime<Utc>>,
    record_start: Option<DateTime<Utc>>,
    to: DateTime<Utc>,
) -> Result<Option<Window>, &'static str> {
    if let Some(from) = stated {
        if from > to {
            return Err("an observation window starts before the verdict it is for");
        }
        return Ok(Some(Window {
            from,
            to,
            source: WindowSource::Stated,
        }));
    }
    let inferred = previous
        .map(|from| (from, WindowSource::PreviousVerdict))
        .or_else(|| record_start.map(|from| (from, WindowSource::RecordStart)));
    Ok(inferred
        .filter(|(from, _)| *from <= to)
        .map(|(from, source)| Window { from, to, source }))
}

/// The collector's facts as they were at capture, labelled so.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collector {
    /// Whether collection was configured on at capture.
    pub enabled: bool,
    /// When the collector last finished a walk of its sources, if it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_walk: Option<DateTime<Utc>>,
}

/// What the retained data showed for the reviewer's own session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observed {
    /// The window read, exactly as queried; absent when none was known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<Window>,
    pub captured_at: DateTime<Utc>,
    /// The earliest usage time retention kept at capture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_from: Option<DateTime<Utc>>,
    pub collector: Collector,
    pub result: ObservedResult,
    /// Why what was found may not be all there was, each said.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub incomplete: Vec<Incomplete>,
}

/// The read's outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ObservedResult {
    /// Samples of the reviewer's own session in the retained data.
    Found(Found),
    /// A completed read found none in that data.
    NoneFound {
        /// Samples attributed to the agent from other sessions.
        other_sessions: u64,
    },
    /// No read could be made or finished: nothing is claimed.
    Unavailable { why: String },
}

/// A completed read's comparison facts, kept whole, apart from what is
/// shown.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Found {
    /// The reviewer's own samples in the window, exactly counted.
    pub samples: u64,
    /// Every provider seen, with its sample count, up to [`MAX_PROVIDERS`].
    pub providers: Vec<ProviderCount>,
    /// More providers were seen than are listed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub providers_truncated: bool,
    /// Samples that named no provider.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unknown_provider: u64,
    /// The provider and model pairs, for display, at most [`MAX_PAIRS`].
    pub pairs: Vec<Configured>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pairs_truncated: bool,
    /// Samples attributed to the agent from other sessions: other
    /// sessions' usage, never the reviewer's own.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub other_sessions: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// One provider and how many of the reviewer's samples named it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCount {
    pub provider: String,
    pub samples: u64,
}

/// The most providers listed in full.
pub const MAX_PROVIDERS: usize = 64;

/// The most provider and model pairs shown.
pub const MAX_PAIRS: usize = 8;

impl Found {
    /// Whether these facts are complete enough for a comparison to say the
    /// providers differ: every sample named one, and the list is whole.
    pub fn eligible_for_different(&self) -> bool {
        !self.providers_truncated && self.unknown_provider == 0
    }
}

/// A reason the observed facts may not be all there was.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Incomplete {
    /// The window starts before what retention kept.
    BeforeRetention,
    /// The collector's last walk ended before the window did: a later
    /// ingestion may add samples.
    CollectorBehind,
    /// Collection was off at capture.
    CollectorOff,
}

/// The reasons that hold for a window, retention and collector.
pub fn incomplete(
    window: Option<&Window>,
    retained_from: Option<DateTime<Utc>>,
    collector: &Collector,
) -> Vec<Incomplete> {
    let mut reasons = Vec::new();
    if let (Some(window), Some(retained)) = (window, retained_from)
        && window.from < retained
    {
        reasons.push(Incomplete::BeforeRetention);
    }
    if let Some(window) = window
        && collector.last_walk.is_none_or(|walk| walk < window.to)
    {
        reasons.push(Incomplete::CollectorBehind);
    }
    if !collector.enabled {
        reasons.push(Incomplete::CollectorOff);
    }
    reasons
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minutes: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + minutes * 60, 0).unwrap()
    }

    #[test]
    fn a_window_is_stated_or_named_as_inferred() {
        let to = at(100);
        let stated = window(Some(at(10)), Some(at(50)), Some(at(0)), to)
            .unwrap()
            .unwrap();
        assert_eq!((stated.from, stated.source), (at(10), WindowSource::Stated));
        assert!(window(Some(at(101)), None, None, to).is_err());
        let previous = window(None, Some(at(50)), Some(at(0)), to)
            .unwrap()
            .unwrap();
        assert_eq!(previous.source, WindowSource::PreviousVerdict);
        let start = window(None, None, Some(at(0)), to).unwrap().unwrap();
        assert_eq!(start.source, WindowSource::RecordStart);
        assert_eq!(window(None, None, None, to).unwrap(), None);
        // An inferred start after the verdict forms no window.
        assert_eq!(window(None, Some(at(200)), None, to).unwrap(), None);
    }

    #[test]
    fn what_may_be_missing_is_said() {
        let w = Window {
            from: at(10),
            to: at(100),
            source: WindowSource::PreviousVerdict,
        };
        let on = Collector {
            enabled: true,
            last_walk: Some(at(120)),
        };
        assert!(incomplete(Some(&w), Some(at(0)), &on).is_empty());
        assert_eq!(
            incomplete(Some(&w), Some(at(20)), &on),
            vec![Incomplete::BeforeRetention]
        );
        let behind = Collector {
            enabled: true,
            last_walk: Some(at(90)),
        };
        assert_eq!(
            incomplete(Some(&w), None, &behind),
            vec![Incomplete::CollectorBehind]
        );
        let off = Collector {
            enabled: false,
            last_walk: Some(at(120)),
        };
        assert_eq!(
            incomplete(Some(&w), None, &off),
            vec![Incomplete::CollectorOff]
        );
    }

    /// A truncated list or a sample with no provider can never support
    /// "different".
    #[test]
    fn only_whole_provider_facts_can_differ() {
        let whole = Found {
            samples: 3,
            providers: vec![ProviderCount {
                provider: "anthropic".into(),
                samples: 3,
            }],
            ..Found::default()
        };
        assert!(whole.eligible_for_different());
        assert!(
            !Found {
                providers_truncated: true,
                ..whole.clone()
            }
            .eligible_for_different()
        );
        assert!(
            !Found {
                unknown_provider: 1,
                ..whole.clone()
            }
            .eligible_for_different()
        );
    }

    #[test]
    fn evidence_round_trips_and_an_old_verdict_has_none() {
        let evidence = ModelEvidence {
            configured: Some(Configured {
                provider: Some("openai".into()),
                model: Some("model-a".into()),
            }),
            session: Some("session-1".into()),
            observed: Some(Observed {
                window: None,
                captured_at: at(0),
                retained_from: None,
                collector: Collector::default(),
                result: ObservedResult::Unavailable {
                    why: "the reviewer's session is not known".into(),
                },
                incomplete: vec![Incomplete::CollectorOff],
            }),
        };
        let text = serde_json::to_string(&evidence).unwrap();
        assert_eq!(
            serde_json::from_str::<ModelEvidence>(&text).unwrap(),
            evidence
        );
        assert_eq!(
            serde_json::from_str::<FindingsPresence>("\"stated\"").unwrap(),
            FindingsPresence::Stated
        );
        assert_eq!(FindingsPresence::default(), FindingsPresence::Unknown);
    }
}
