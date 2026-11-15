//! What a `desktop` command's report says happened, as a word.
//!
//! `agentdocker desktop update`, `install` and `rollback` print one JSON
//! report each, and a reader told what had happened from which keys the
//! report had: an update that was not published from one that was up to
//! date by `published`, a preview from an apply by `preview` and whether
//! there was a `daemon` object, a daemon that reloaded and answers from
//! one that reloaded and went silent by whether `serving` was null. The
//! last two looked the same to a reader that asked only `reloaded`.
//!
//! The words here are added beside those keys, which stay as they are:
//! `update.outcome` for the update, and `daemon.outcome` for the handover
//! to a running daemon after an activation. Each is computed from the
//! same facts the keys carry, by one function, so the word and the keys
//! cannot disagree. A reader that does not know a word shows it as
//! printed; a report without one (an earlier build's) is read by its
//! keys, as before.
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// What an update command found or did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UpdateOutcome {
    /// No release has been published to the channel for this machine.
    NotPublished,
    /// The published release is not newer than what is installed.
    UpToDate,
    /// A newer release is published; nothing was downloaded.
    Available,
    /// A newer release is published and it is a preview build, which is
    /// downloaded only with consent to preview builds.
    NeedsConsent,
    /// The newer release was downloaded, verified and previewed; nothing
    /// was installed.
    Previewed,
    /// The newer release was installed.
    Applied,
}

/// How far an update command went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateStep {
    /// It read the feed and stopped: a check, or nothing newer to fetch.
    Checked,
    /// It downloaded, verified and previewed.
    Previewed,
    /// It installed.
    Applied,
}

/// What an update report already says, in its own keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpdateFacts {
    /// `published`: a release exists for this machine.
    pub published: bool,
    /// `update_available`: it is newer than what is installed.
    pub available: bool,
    /// `preview_consent_required`: it is a preview build and consent to
    /// preview builds was not given.
    pub consent_required: bool,
    pub step: UpdateStep,
}

impl UpdateOutcome {
    pub const ALL: [UpdateOutcome; 6] = [
        UpdateOutcome::NotPublished,
        UpdateOutcome::UpToDate,
        UpdateOutcome::Available,
        UpdateOutcome::NeedsConsent,
        UpdateOutcome::Previewed,
        UpdateOutcome::Applied,
    ];

    /// The printed word.
    pub const fn as_str(self) -> &'static str {
        match self {
            UpdateOutcome::NotPublished => "not_published",
            UpdateOutcome::UpToDate => "up_to_date",
            UpdateOutcome::Available => "available",
            UpdateOutcome::NeedsConsent => "needs_consent",
            UpdateOutcome::Previewed => "previewed",
            UpdateOutcome::Applied => "applied",
        }
    }

    /// The outcome the facts allow. Nothing is published before anything
    /// is newer, and nothing newer is fetched before consent: a command
    /// that went on to preview or install had what it needed.
    pub const fn of(facts: UpdateFacts) -> Self {
        if !facts.published {
            return UpdateOutcome::NotPublished;
        }
        if !facts.available {
            return UpdateOutcome::UpToDate;
        }
        match facts.step {
            UpdateStep::Applied => UpdateOutcome::Applied,
            UpdateStep::Previewed => UpdateOutcome::Previewed,
            UpdateStep::Checked if facts.consent_required => UpdateOutcome::NeedsConsent,
            UpdateStep::Checked => UpdateOutcome::Available,
        }
    }

    /// Whether a newer release is still there to be fetched or applied.
    pub const fn offers(self) -> bool {
        matches!(
            self,
            UpdateOutcome::Available | UpdateOutcome::NeedsConsent | UpdateOutcome::Previewed
        )
    }
}

/// What became of the running daemon after a release was activated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Handover {
    /// No daemon answered: none needed reloading, and the next launch
    /// starts the activated release.
    NoDaemon,
    /// The activation left the active release as it was, so there was
    /// nothing to hand over to.
    Unchanged,
    /// The daemon reloaded, and the activated release's daemon answers.
    Reloaded,
    /// The daemon accepted the reload and nothing answers afterwards: not
    /// a success, and not known to be a failure.
    ReloadedSilent,
    /// The daemon refused to reload and keeps serving the earlier
    /// release.
    Refused,
    /// What happened is not known: the daemon could not be asked, or its
    /// answer to the reload was lost or not understood.
    Unknown,
}

/// What a handover report already says, in its own keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandoverFacts {
    /// The activation moved to another release.
    pub changed: bool,
    /// `answered`: a daemon answered before the reload; `None` when it
    /// could not be asked at all.
    pub answered: Option<bool>,
    /// `reloaded`: the reload was accepted (`true`) or refused (`false`);
    /// `None` when the reply was lost or not understood, or none was
    /// asked for.
    pub reloaded: Option<bool>,
    /// `serving` is not null: a daemon answered afterwards.
    pub serving: bool,
}

impl Handover {
    pub const ALL: [Handover; 6] = [
        Handover::NoDaemon,
        Handover::Unchanged,
        Handover::Reloaded,
        Handover::ReloadedSilent,
        Handover::Refused,
        Handover::Unknown,
    ];

    /// The printed word.
    pub const fn as_str(self) -> &'static str {
        match self {
            Handover::NoDaemon => "no_daemon",
            Handover::Unchanged => "unchanged",
            Handover::Reloaded => "reloaded",
            Handover::ReloadedSilent => "reloaded_silent",
            Handover::Refused => "refused",
            Handover::Unknown => "unknown",
        }
    }

    /// The outcome the facts allow.
    pub const fn of(facts: HandoverFacts) -> Self {
        if !facts.changed {
            return Handover::Unchanged;
        }
        match (facts.answered, facts.reloaded) {
            (None, _) => Handover::Unknown,
            (Some(false), _) => Handover::NoDaemon,
            (Some(true), Some(true)) if facts.serving => Handover::Reloaded,
            (Some(true), Some(true)) => Handover::ReloadedSilent,
            (Some(true), Some(false)) => Handover::Refused,
            (Some(true), None) => Handover::Unknown,
        }
    }

    /// Whether the activated release's daemon is known to be serving.
    pub const fn switched(self) -> bool {
        matches!(self, Handover::Reloaded)
    }
}

/// A word that is none of a report's outcomes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownWord(pub String);

impl fmt::Display for UnknownWord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown outcome {:?}", self.0)
    }
}

impl std::error::Error for UnknownWord {}

macro_rules! word {
    ($name:ident) => {
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = UnknownWord;

            fn from_str(word: &str) -> Result<Self, Self::Err> {
                Self::ALL
                    .into_iter()
                    .find(|outcome| outcome.as_str() == word)
                    .ok_or_else(|| UnknownWord(word.to_owned()))
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                String::deserialize(deserializer)?
                    .parse()
                    .map_err(serde::de::Error::custom)
            }
        }
    };
}

word!(UpdateOutcome);
word!(Handover);

#[cfg(test)]
mod tests {
    use super::*;

    /// Every combination of what an update report says, and the one word
    /// for it.
    #[test]
    fn the_update_outcome_follows_from_the_report() {
        use UpdateOutcome::*;
        use UpdateStep as Step;
        let rows: &[(bool, bool, bool, UpdateStep, UpdateOutcome)] = &[
            // Nothing published: nothing else is asked.
            (false, false, false, Step::Checked, NotPublished),
            (false, true, true, Step::Applied, NotPublished),
            // Published and not newer.
            (true, false, false, Step::Checked, UpToDate),
            (true, false, true, Step::Previewed, UpToDate),
            // Newer, and the command only looked.
            (true, true, false, Step::Checked, Available),
            (true, true, true, Step::Checked, NeedsConsent),
            // Newer, and the command went on: it had consent, whatever
            // the key computed before it says.
            (true, true, false, Step::Previewed, Previewed),
            (true, true, true, Step::Previewed, Previewed),
            (true, true, false, Step::Applied, Applied),
        ];
        for &(published, available, consent_required, step, expected) in rows {
            let facts = UpdateFacts {
                published,
                available,
                consent_required,
                step,
            };
            assert_eq!(UpdateOutcome::of(facts), expected, "{facts:?}");
        }
        assert!(Available.offers() && NeedsConsent.offers() && Previewed.offers());
        assert!(!UpToDate.offers() && !NotPublished.offers() && !Applied.offers());
    }

    /// Every combination of what a handover report says, and the one
    /// word for it. A reload that was accepted and is followed by silence
    /// is its own word: it was read as a reload that worked.
    #[test]
    fn the_handover_follows_from_the_report() {
        use Handover::*;
        /// The activation changed the release; a daemon answered; the
        /// reload was accepted; a daemon answers afterwards; the word.
        type Row = (bool, Option<bool>, Option<bool>, bool, Handover);
        let rows: &[Row] = &[
            // The release was already active: nothing to hand over,
            // whatever answers.
            (false, Some(true), Some(false), true, Unchanged),
            (false, Some(false), Some(false), false, Unchanged),
            // It could not be asked at all.
            (true, None, None, false, Unknown),
            // Nothing answered the first question.
            (true, Some(false), None, false, NoDaemon),
            // It reloaded.
            (true, Some(true), Some(true), true, Reloaded),
            (true, Some(true), Some(true), false, ReloadedSilent),
            // It refused, and keeps serving.
            (true, Some(true), Some(false), true, Refused),
            (true, Some(true), Some(false), false, Refused),
            // Its reply was lost: whatever serves now, nothing is known.
            (true, Some(true), None, true, Unknown),
            (true, Some(true), None, false, Unknown),
        ];
        for &(changed, answered, reloaded, serving, expected) in rows {
            let facts = HandoverFacts {
                changed,
                answered,
                reloaded,
                serving,
            };
            assert_eq!(Handover::of(facts), expected, "{facts:?}");
        }
        assert!(Reloaded.switched());
        for other in [NoDaemon, Unchanged, ReloadedSilent, Refused, Unknown] {
            assert!(!other.switched(), "{other}");
        }
    }

    /// The words are what is printed and what is read back; a word that
    /// is not one of them is refused, not guessed at.
    #[test]
    fn each_word_is_printed_and_read_back_and_no_other_is_read() {
        let update: Vec<&str> = UpdateOutcome::ALL.iter().map(|o| o.as_str()).collect();
        assert_eq!(
            update,
            [
                "not_published",
                "up_to_date",
                "available",
                "needs_consent",
                "previewed",
                "applied"
            ]
        );
        let handover: Vec<&str> = Handover::ALL.iter().map(|o| o.as_str()).collect();
        assert_eq!(
            handover,
            [
                "no_daemon",
                "unchanged",
                "reloaded",
                "reloaded_silent",
                "refused",
                "unknown"
            ]
        );
        for outcome in UpdateOutcome::ALL {
            let printed = serde_json::to_string(&outcome).unwrap();
            assert_eq!(printed, format!("\"{}\"", outcome.as_str()));
            assert_eq!(
                serde_json::from_str::<UpdateOutcome>(&printed).unwrap(),
                outcome
            );
            assert_eq!(outcome.to_string().parse::<UpdateOutcome>(), Ok(outcome));
        }
        for outcome in Handover::ALL {
            let printed = serde_json::to_string(&outcome).unwrap();
            assert_eq!(serde_json::from_str::<Handover>(&printed).unwrap(), outcome);
            assert_eq!(outcome.to_string().parse::<Handover>(), Ok(outcome));
        }
        assert_eq!(
            "installed".parse::<UpdateOutcome>(),
            Err(UnknownWord("installed".into()))
        );
        assert!(serde_json::from_str::<Handover>("\"restarted\"").is_err());
    }
}
