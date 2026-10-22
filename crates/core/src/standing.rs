//! Where an agent stands with the person: one answer per agent, from one
//! function, used by every view that shows an agent.
//!
//! Every agent app studied folds its signals into a few states with a
//! fixed precedence; the loudest state that applies wins, so a row never
//! carries two. The order is the attention order: sorting by [`Standing`]
//! puts what needs the person first.

use crate::Activity;

/// Why an agent needs the person.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Need {
    /// It asked a question in AgentDocker that is still open.
    Question,
    /// Its provider stopped for the person in its own terminal: a
    /// permission prompt or a question dialog.
    Prompt,
    /// Messages to it are held: it is not receiving them, its provider is
    /// blocked, or it ended with messages still queued.
    Delivery,
}

impl Need {
    /// The words a row shows for it.
    pub fn word(self) -> &'static str {
        match self {
            Self::Question => "needs input",
            Self::Prompt => "waiting for you",
            Self::Delivery => "not receiving messages",
        }
    }
}

/// One agent's standing, loudest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Standing {
    /// The person has to act.
    NeedsYou(Need),
    /// It failed, and the person has not looked since.
    Failed,
    /// Its provider reports a turn in progress.
    Working,
    /// It waits on something other than the person: a lease another agent
    /// holds. Kept apart from needs-you so it never asks for attention.
    Parked,
    /// Launched, not yet ready for input.
    Starting,
    /// It finished something the person has not looked at.
    FinishedUnseen,
    /// Live and quiet, or nothing is known.
    Idle,
    /// It has ended, with nothing left for the person.
    Ended,
}

/// What a view knows about an agent, gathered from the records and the
/// person's own state (questions, what they have looked at).
#[derive(Clone, Copy, Debug, Default)]
pub struct Signals<'a> {
    pub live: bool,
    /// It ended with a failure.
    pub failed: bool,
    /// The person has opened it since it failed or finished.
    pub seen: bool,
    /// An open question from it waits on the person.
    pub question: bool,
    /// Messages to it are held and the person can do something about it.
    pub delivery_held: bool,
    /// It finished a turn the person has not looked at.
    pub finished_unseen: bool,
    pub activity: Option<&'a Activity>,
}

impl Standing {
    pub fn of(signals: Signals<'_>) -> Self {
        if signals.question {
            return Self::NeedsYou(Need::Question);
        }
        if signals.live && matches!(signals.activity, Some(Activity::Waiting { .. })) {
            return Self::NeedsYou(Need::Prompt);
        }
        if signals.delivery_held {
            return Self::NeedsYou(Need::Delivery);
        }
        if signals.failed {
            return if signals.seen {
                Self::Ended
            } else {
                Self::Failed
            };
        }
        if !signals.live {
            // A session that finished while the person was away is news
            // until they look, ended or not.
            return if signals.finished_unseen {
                Self::FinishedUnseen
            } else {
                Self::Ended
            };
        }
        match signals.activity {
            Some(Activity::Working { .. }) => Self::Working,
            Some(Activity::Blocked { .. }) => Self::Parked,
            Some(Activity::Starting) => Self::Starting,
            _ if signals.finished_unseen => Self::FinishedUnseen,
            _ => Self::Idle,
        }
    }

    pub fn needs_you(self) -> bool {
        matches!(self, Self::NeedsYou(_))
    }

    /// Whether the row's name is set heavier: something there is new to
    /// the person or waits on them.
    pub fn calls_for_attention(self) -> bool {
        matches!(
            self,
            Self::NeedsYou(_) | Self::Failed | Self::FinishedUnseen
        )
    }

    /// The state in full, for searching and for the detail under a
    /// [`label`](Self::label) that says less than this.
    pub fn word(self) -> &'static str {
        match self {
            Self::NeedsYou(need) => need.word(),
            Self::Failed => "failed",
            Self::Working => "working",
            Self::Parked => "waiting on a lease",
            Self::Starting => "starting",
            Self::FinishedUnseen => "finished since you looked",
            Self::Idle => "idle",
            Self::Ended => "ended",
        }
    }

    /// The state on its chip: a word or two every row shows, healthy or
    /// not. Where it says less than [`word`](Self::word) — which need, or
    /// since when — the word is the chip's detail.
    pub fn label(self) -> &'static str {
        match self {
            Self::NeedsYou(_) => "Needs you",
            Self::Failed => "Failed",
            Self::Working => "Working",
            Self::Parked => "Waiting on a lease",
            Self::Starting => "Starting",
            Self::FinishedUnseen => "Finished",
            Self::Idle => "Idle",
            Self::Ended => "Ended",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn live(activity: &Activity) -> Signals<'_> {
        Signals {
            live: true,
            activity: Some(activity),
            ..Signals::default()
        }
    }

    #[test]
    fn the_loudest_state_wins_and_idle_says_nothing() {
        let now = Utc::now();
        let working = Activity::Working { since: now };
        let waiting = Activity::Waiting {
            since: now,
            reason: None,
        };
        let idle = Activity::Idle { since: now };
        let blocked = Activity::Blocked {
            resource: crate::ResourceKey::new("task:x"),
            held_by: Vec::new(),
            since: now,
        };
        assert_eq!(Standing::of(live(&working)), Standing::Working);
        assert_eq!(Standing::of(live(&idle)), Standing::Idle);
        assert_eq!(Standing::of(live(&blocked)), Standing::Parked);
        assert_eq!(Standing::of(live(&Activity::Starting)), Standing::Starting);
        assert_eq!(
            Standing::of(live(&waiting)),
            Standing::NeedsYou(Need::Prompt)
        );
        // A question beats everything, even a turn in progress.
        assert_eq!(
            Standing::of(Signals {
                question: true,
                ..live(&working)
            }),
            Standing::NeedsYou(Need::Question)
        );
        assert_eq!(
            Standing::of(Signals {
                delivery_held: true,
                ..live(&working)
            }),
            Standing::NeedsYou(Need::Delivery)
        );
        // A finished turn is news only while the agent is quiet: a running
        // agent stays Working however much is unseen.
        assert_eq!(
            Standing::of(Signals {
                finished_unseen: true,
                ..live(&working)
            }),
            Standing::Working
        );
        assert_eq!(
            Standing::of(Signals {
                finished_unseen: true,
                ..live(&idle)
            }),
            Standing::FinishedUnseen
        );
    }

    #[test]
    fn a_failure_is_loud_until_seen_and_an_ended_agent_is_quiet() {
        let failed = Signals {
            failed: true,
            ..Signals::default()
        };
        assert_eq!(Standing::of(failed), Standing::Failed);
        assert_eq!(
            Standing::of(Signals {
                seen: true,
                ..failed
            }),
            Standing::Ended
        );
        assert_eq!(Standing::of(Signals::default()), Standing::Ended);
        assert_eq!(
            Standing::of(Signals {
                finished_unseen: true,
                ..Signals::default()
            }),
            Standing::FinishedUnseen,
            "an ended session finished while away is still news"
        );
        // Held messages on an ended agent still need the person.
        assert_eq!(
            Standing::of(Signals {
                delivery_held: true,
                ..Signals::default()
            }),
            Standing::NeedsYou(Need::Delivery)
        );
    }

    #[test]
    fn sorting_puts_what_needs_the_person_first() {
        let mut all = vec![
            Standing::Ended,
            Standing::Idle,
            Standing::FinishedUnseen,
            Standing::Starting,
            Standing::Parked,
            Standing::Working,
            Standing::Failed,
            Standing::NeedsYou(Need::Delivery),
            Standing::NeedsYou(Need::Question),
        ];
        all.sort();
        assert_eq!(all[0], Standing::NeedsYou(Need::Question));
        assert_eq!(all[2], Standing::Failed);
        assert_eq!(all.last(), Some(&Standing::Ended));
        assert!(Standing::FinishedUnseen.calls_for_attention());
        assert!(!Standing::Working.calls_for_attention());
    }

    /// Every state has a short label, and the label never contradicts the
    /// full word: where they differ, the word only says more.
    #[test]
    fn a_label_is_the_word_or_its_head() {
        let all = [
            Standing::NeedsYou(Need::Question),
            Standing::NeedsYou(Need::Prompt),
            Standing::NeedsYou(Need::Delivery),
            Standing::Failed,
            Standing::Working,
            Standing::Parked,
            Standing::Starting,
            Standing::FinishedUnseen,
            Standing::Idle,
            Standing::Ended,
        ];
        for standing in all {
            let label = standing.label();
            assert!(!label.is_empty() && label.len() <= 18, "{label}");
            assert!(label.starts_with(char::is_uppercase), "{label}");
            if !standing.needs_you() {
                assert!(
                    standing.word().starts_with(&label.to_lowercase()),
                    "{label} / {}",
                    standing.word()
                );
            }
        }
        assert_eq!(Standing::FinishedUnseen.label(), "Finished");
        assert_eq!(Standing::NeedsYou(Need::Delivery).label(), "Needs you");
    }
}
