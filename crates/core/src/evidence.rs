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

use crate::usage::report::{Collection, CollectionPhase};
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
    /// Given by a client or daemon from before this was kept, or a word a
    /// newer build wrote: the counts are as recorded, and whether they were
    /// stated cannot be told.
    #[default]
    #[serde(other)]
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
    /// A word a newer build wrote.
    #[serde(other)]
    Unknown,
}

/// The verdict a window's start was taken from: its id, its place in the
/// project's order, and the boundary it gave: its `captured_at`, the
/// sample-time bound fixed when its capture began (its time, for a verdict
/// that kept no capture).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Predecessor {
    pub id: String,
    pub seq: u64,
    pub boundary: DateTime<Utc>,
}

/// The samples read: those whose own time is in `[from, to)`. `to` is
/// `captured_at`, the sample-time upper bound fixed when the capture
/// began; a next round inferred from this one starts exactly there, so two
/// directly linked rounds meet with no sample in neither or both. Rounds
/// captured before either was kept both start from the same predecessor,
/// and their windows overlap, which `previous` shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub source: WindowSource,
    /// The verdict `from` was taken from, for a `previous_verdict` window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<Predecessor>,
}

/// The window for a verdict given at `to`: the reviewer's stated start,
/// else its previous verdict on the branch, else its record's start;
/// `None` when none is known. A stated start after the verdict is refused.
pub fn window(
    stated: Option<DateTime<Utc>>,
    previous: Option<Predecessor>,
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
            previous: None,
        }));
    }
    let inferred = match previous {
        Some(previous) => Some((
            previous.boundary,
            WindowSource::PreviousVerdict,
            Some(previous),
        )),
        None => record_start.map(|from| (from, WindowSource::RecordStart, None)),
    };
    Ok(inferred
        .filter(|(from, _, _)| *from <= to)
        .map(|(from, source, previous)| Window {
            from,
            to,
            source,
            previous,
        }))
}

/// The collector's facts as they were at capture, labelled so; each
/// absent when it could not be read, which is never the same as off.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collector {
    /// Whether collection was configured on at capture; absent when the
    /// configuration could not be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// The collection's phase as [`Collection::phase`] reads it from the
    /// collection and the configuration (`discovering`, `discovery
    /// incomplete`, `reading`, `caught up`, …); absent when no collection
    /// was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_complete: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_files: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_tail_files: Option<u64>,
    /// When the collector last finished a walk of its sources, if it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_walk: Option<DateTime<Utc>>,
}

impl Collector {
    /// The facts of `collection` as read, with whether collection was
    /// configured on (`None` when that could not be read).
    pub fn observed(collection: &Collection, enabled: Option<bool>) -> Collector {
        let phase = Collection {
            enabled,
            ..collection.clone()
        }
        .phase();
        Collector {
            enabled,
            phase: Some(phase.word().to_owned()),
            discovery_complete: Some(collection.discovery_complete),
            pending_files: collection.pending_files,
            pending_tail_files: collection.pending_tail_files,
            last_walk: collection.completed_at,
        }
    }
}

/// What the retained data showed for the reviewer's own session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observed {
    /// The window read, exactly as queried; absent when none was known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<Window>,
    /// The sample-time upper bound, fixed when the capture began (before
    /// the read's transaction).
    pub captured_at: DateTime<Utc>,
    /// The earliest usage time retention kept at capture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_from: Option<DateTime<Utc>>,
    /// The lower bound actually read: the window's start, or the retention
    /// cutoff when that is later, so an expired sample not yet pruned is
    /// never counted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_from: Option<DateTime<Utc>>,
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
    /// A word a newer build wrote: nothing is claimed.
    #[serde(other)]
    Unknown,
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
    /// Whether these facts are complete: every sample named a provider,
    /// and the list is whole. Only completeness: whether a provider string
    /// is a known vendor is the comparison's to judge, from the strings
    /// kept as recorded.
    pub fn complete(&self) -> bool {
        !self.providers_truncated && self.unknown_provider == 0
    }
}

/// A reason the observed facts may not be all there was.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Incomplete {
    /// The window starts before what retention kept.
    BeforeRetention,
    /// No completed walk was recorded at or after the end of the window:
    /// one that ended earlier, or none yet (a first walk may still be under
    /// way). A later ingestion may add samples.
    CollectorBehind,
    /// The collector had not caught up: starting, still walking its roots,
    /// a walk that stopped early, or files still being read.
    CollectorPending,
    /// What the collector had read is not known: no collection was read.
    CollectorUnknown,
    /// Collection was off at capture.
    CollectorOff,
    /// Whether collection was on could not be read.
    EnabledUnknown,
    /// A word a newer build wrote.
    #[serde(other)]
    Unknown,
}

impl Incomplete {
    pub fn said(self) -> &'static str {
        match self {
            Incomplete::BeforeRetention => "the window starts before retention",
            Incomplete::CollectorBehind => {
                "no completed walk was recorded at or after the end of the window"
            }
            Incomplete::CollectorPending => "the collector had not caught up",
            Incomplete::CollectorUnknown => "what the collector had read is not known",
            Incomplete::CollectorOff => "collection was off",
            Incomplete::EnabledUnknown => "whether collection was on is unknown",
            Incomplete::Unknown => "an unknown limitation",
        }
    }
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
    // A delay is said only from what was read; with no collection read,
    // only that it is not known.
    let word = |phase: CollectionPhase| phase.word();
    match collector.phase.as_deref() {
        None => reasons.push(Incomplete::CollectorUnknown),
        Some(phase) if phase == word(CollectionPhase::Unknown) => {
            reasons.push(Incomplete::CollectorUnknown);
        }
        Some(phase) => {
            if let Some(window) = window
                && collector.last_walk.is_none_or(|walk| walk < window.to)
            {
                reasons.push(Incomplete::CollectorBehind);
            }
            let caught_up = phase == word(CollectionPhase::CaughtUp { at: None });
            if !caught_up && phase != word(CollectionPhase::Off) {
                reasons.push(Incomplete::CollectorPending);
            }
        }
    }
    match collector.enabled {
        Some(true) => {}
        Some(false) => reasons.push(Incomplete::CollectorOff),
        None => reasons.push(Incomplete::EnabledUnknown),
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
        let before = |minutes| Predecessor {
            id: "v1".into(),
            seq: 4,
            boundary: at(minutes),
        };
        let stated = window(Some(at(10)), Some(before(50)), Some(at(0)), to)
            .unwrap()
            .unwrap();
        assert_eq!(
            (stated.from, stated.source, stated.previous),
            (at(10), WindowSource::Stated, None)
        );
        assert!(window(Some(at(101)), None, None, to).is_err());
        // The previous round's boundary, and which round gave it.
        let previous = window(None, Some(before(50)), Some(at(0)), to)
            .unwrap()
            .unwrap();
        assert_eq!(
            (previous.from, previous.source, previous.previous),
            (at(50), WindowSource::PreviousVerdict, Some(before(50)))
        );
        let start = window(None, None, Some(at(0)), to).unwrap().unwrap();
        assert_eq!(start.source, WindowSource::RecordStart);
        assert_eq!(window(None, None, None, to).unwrap(), None);
        // An inferred start after the verdict forms no window.
        assert_eq!(window(None, Some(before(200)), None, to).unwrap(), None);
    }

    #[test]
    fn what_may_be_missing_is_said() {
        use crate::usage::report::CollectionState;
        let w = Window {
            from: at(10),
            to: at(100),
            source: WindowSource::PreviousVerdict,
            previous: None,
        };
        let caught = Collection {
            state: CollectionState::CaughtUp,
            discovery_generation: Some(1),
            completed_at: Some(at(120)),
            discovery_complete: true,
            discovery_ended: true,
            pending_files: Some(0),
            pending_tail_files: Some(0),
            ..Collection::default()
        };
        let on = Collector::observed(&caught, Some(true));
        assert_eq!(on.phase.as_deref(), Some("caught up"));
        assert!(incomplete(Some(&w), Some(at(0)), &on).is_empty());
        assert_eq!(
            incomplete(Some(&w), Some(at(20)), &on),
            vec![Incomplete::BeforeRetention]
        );
        let behind = Collector {
            last_walk: Some(at(90)),
            ..on.clone()
        };
        assert_eq!(
            incomplete(Some(&w), None, &behind),
            vec![Incomplete::CollectorBehind]
        );
        // A walk under way and one that stopped early, otherwise the same
        // fields, are told apart; neither, nor a pass still reading files,
        // has caught up, however recent the last walk.
        let walking = Collection {
            state: CollectionState::Unknown,
            discovery_ended: false,
            discovery_complete: false,
            ..caught.clone()
        };
        let stopped = Collection {
            discovery_ended: true,
            ..walking.clone()
        };
        let reading = Collection {
            state: CollectionState::Scanning,
            pending_files: Some(3),
            ..caught.clone()
        };
        let (walking, stopped, reading) = (
            Collector::observed(&walking, Some(true)),
            Collector::observed(&stopped, Some(true)),
            Collector::observed(&reading, Some(true)),
        );
        assert_eq!(
            (walking.phase.as_deref(), stopped.phase.as_deref()),
            (Some("discovering"), Some("discovery incomplete"))
        );
        for pending in [walking, stopped, reading] {
            assert_eq!(
                incomplete(Some(&w), None, &pending),
                vec![Incomplete::CollectorPending],
                "{pending:?}"
            );
        }
        // A first walk still under way has completed none: no completed
        // walk at or after the window's end, said so, and not caught up.
        let first = Collector::observed(
            &Collection {
                state: CollectionState::Unknown,
                completed_at: None,
                discovery_ended: false,
                discovery_complete: false,
                ..caught.clone()
            },
            Some(true),
        );
        assert_eq!(
            (first.phase.as_deref(), first.last_walk),
            (Some("discovering"), None)
        );
        assert_eq!(
            incomplete(Some(&w), None, &first),
            vec![Incomplete::CollectorBehind, Incomplete::CollectorPending]
        );
        assert_eq!(
            Incomplete::CollectorBehind.said(),
            "no completed walk was recorded at or after the end of the window"
        );
        // A read that took no collection (one stopped at its deadline, say)
        // says only that what the collector had read is not known.
        let unread = Collector {
            enabled: Some(true),
            ..Collector::default()
        };
        assert_eq!(
            incomplete(Some(&w), None, &unread),
            vec![Incomplete::CollectorUnknown]
        );
        assert_eq!(
            incomplete(Some(&w), None, &Collector::observed(&caught, Some(false))),
            vec![Incomplete::CollectorOff]
        );
        // A configuration that could not be read is not "off".
        assert_eq!(
            incomplete(Some(&w), None, &Collector::observed(&caught, None)),
            vec![Incomplete::EnabledUnknown]
        );
    }

    /// A truncated list or a sample with no provider is never complete;
    /// whether a complete list's providers are known vendors is the
    /// comparison's to judge.
    #[test]
    fn only_whole_provider_facts_are_complete() {
        let whole = Found {
            samples: 3,
            providers: vec![ProviderCount {
                provider: "anthropic".into(),
                samples: 3,
            }],
            ..Found::default()
        };
        assert!(whole.complete());
        assert!(
            !Found {
                providers_truncated: true,
                ..whole.clone()
            }
            .complete()
        );
        assert!(
            !Found {
                unknown_provider: 1,
                ..whole.clone()
            }
            .complete()
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
                read_from: None,
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

    /// A word a newer build writes reads as unknown, never as a decoding
    /// failure that would stop every read of the verdicts beside it.
    #[test]
    fn a_newer_word_reads_as_unknown() {
        assert_eq!(
            serde_json::from_str::<FindingsPresence>("\"estimated\"").unwrap(),
            FindingsPresence::Unknown
        );
        let observed: Observed = serde_json::from_value(serde_json::json!({
            "window": {"from": at(0), "to": at(1), "source": "from_a_card"},
            "captured_at": at(1),
            "collector": {},
            "result": {"result": "sampled", "share": 3},
            "incomplete": ["clock_skew", "collector_off"],
        }))
        .unwrap();
        assert_eq!(observed.window.unwrap().source, WindowSource::Unknown);
        assert_eq!(observed.result, ObservedResult::Unknown);
        assert_eq!(
            observed.incomplete,
            vec![Incomplete::Unknown, Incomplete::CollectorOff]
        );
    }
}
