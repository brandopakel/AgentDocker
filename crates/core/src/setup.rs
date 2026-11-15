//! The lifecycle of a saved setup plan (`agentdocker setup`): the phases a
//! receipt is stored in, and the one table of moves between them.
//!
//! The words are the stored and the printed form: a receipt's `phase` on
//! disk, and the `phase` of `setup --json`, `--show` and `--list`. They do
//! not change. A word this table does not know is refused, not guessed at:
//! a receipt that carries one cannot be read.
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Where a saved setup plan stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Saved for review; nothing of it has been written.
    Prepared,
    /// An apply recorded its start and not its end: it is running, or it
    /// was interrupted and the plan is partly written.
    Applying,
    /// Everything the plan writes is in place.
    Applied,
    /// An undo recorded its start and not its end: it is running, or it
    /// was interrupted and the plan is partly taken back.
    Undoing,
    /// Everything is as it was before the plan. Terminal: the snapshots
    /// are of a state that has passed, so it is never applied again.
    Undone,
}

/// What a plan's files hold while its receipt is at rest in a phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Files {
    /// Each file as the plan found it.
    Before,
    /// Each file as the plan leaves it.
    After,
    /// Each file one or the other: only a receipt that recorded a start
    /// can explain a mix.
    Either,
}

/// What an apply or an undo does to a plan in a phase that allows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// It is already where the move ends: nothing is written, the receipt
    /// included.
    Stay,
    /// Nothing of the plan was written, so only the receipt moves.
    Record(Phase),
    /// Record `through` durably, do the work, then record `to`.
    Work { through: Phase, to: Phase },
}

/// Why a plan in some phase cannot be applied. An undo is never refused
/// for the phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// It was undone: its snapshots are stale, and a fresh preview is the
    /// way to apply again.
    Undone,
    /// Its undo started and did not finish. It is half undone, and
    /// finishing the undo is what settles it.
    UndoUnfinished,
}

impl Phase {
    /// Every phase, in the order a plan can pass through them.
    pub const ALL: [Phase; 5] = [
        Phase::Prepared,
        Phase::Applying,
        Phase::Applied,
        Phase::Undoing,
        Phase::Undone,
    ];

    /// The stored and printed word.
    pub const fn as_str(self) -> &'static str {
        match self {
            Phase::Prepared => "prepared",
            Phase::Applying => "applying",
            Phase::Applied => "applied",
            Phase::Undoing => "undoing",
            Phase::Undone => "undone",
        }
    }

    /// What the plan's files must hold for a receipt at rest in this phase.
    pub const fn files(self) -> Files {
        match self {
            Phase::Prepared | Phase::Undone => Files::Before,
            Phase::Applied => Files::After,
            Phase::Applying | Phase::Undoing => Files::Either,
        }
    }

    /// Whether a start was recorded without its end. A receipt at rest in
    /// such a phase with nothing running on it was interrupted.
    pub const fn in_progress(self) -> bool {
        matches!(self.files(), Files::Either)
    }

    /// The transition table: what an apply (`undo` false) or an undo does
    /// from this phase, or why it may not.
    pub const fn next(self, undo: bool) -> Result<Step, Refusal> {
        match (self, undo) {
            (Phase::Prepared | Phase::Applying, false) => Ok(Step::Work {
                through: Phase::Applying,
                to: Phase::Applied,
            }),
            (Phase::Applied, false) => Ok(Step::Stay),
            (Phase::Undoing, false) => Err(Refusal::UndoUnfinished),
            (Phase::Undone, false) => Err(Refusal::Undone),
            (Phase::Prepared, true) => Ok(Step::Record(Phase::Undone)),
            (Phase::Applying | Phase::Applied | Phase::Undoing, true) => Ok(Step::Work {
                through: Phase::Undoing,
                to: Phase::Undone,
            }),
            (Phase::Undone, true) => Ok(Step::Stay),
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A phase word that names no phase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownPhase(pub String);

impl fmt::Display for UnknownPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown phase `{}`", self.0)
    }
}

impl std::error::Error for UnknownPhase {}

impl FromStr for Phase {
    type Err = UnknownPhase;

    fn from_str(word: &str) -> Result<Self, Self::Err> {
        Phase::ALL
            .into_iter()
            .find(|phase| phase.as_str() == word)
            .ok_or_else(|| UnknownPhase(word.to_owned()))
    }
}

impl Serialize for Phase {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// A phase is read from a receipt, so a word that names none is said as
/// what it makes of the receipt: unreadable.
impl<'de> Deserialize<'de> for Phase {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(|unknown: UnknownPhase| {
                serde::de::Error::custom(format_args!("unreadable setup receipt: {unknown}"))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every (phase, move) pair, pinned: the moves that are legal, where
    /// each goes, and the two an apply is refused for.
    #[test]
    fn every_phase_and_move_has_one_pinned_outcome() {
        use Phase::*;
        let applies = Ok(Step::Work {
            through: Applying,
            to: Applied,
        });
        let undoes = Ok(Step::Work {
            through: Undoing,
            to: Undone,
        });
        let table = [
            (Prepared, false, applies),
            (Prepared, true, Ok(Step::Record(Undone))),
            (Applying, false, applies),
            (Applying, true, undoes),
            (Applied, false, Ok(Step::Stay)),
            (Applied, true, undoes),
            (Undoing, false, Err(Refusal::UndoUnfinished)),
            (Undoing, true, undoes),
            (Undone, false, Err(Refusal::Undone)),
            (Undone, true, Ok(Step::Stay)),
        ];
        for (phase, undo, outcome) in table {
            assert_eq!(phase.next(undo), outcome, "{phase}, undo={undo}");
        }
        for phase in Phase::ALL {
            for undo in [false, true] {
                assert!(
                    table.iter().any(|row| (row.0, row.1) == (phase, undo)),
                    "{phase}, undo={undo} is not in the table"
                );
            }
        }
    }

    /// An undo is never refused for the phase, undone is the end, and a
    /// move's work is recorded through the in-progress phase of its own
    /// direction.
    #[test]
    fn undone_is_terminal_and_work_passes_through_its_own_in_progress_phase() {
        for phase in Phase::ALL {
            assert!(phase.next(true).is_ok(), "{phase}");
            for undo in [false, true] {
                if let Ok(Step::Work { through, to }) = phase.next(undo) {
                    assert!(through.in_progress() && !to.in_progress());
                    assert_eq!(through.next(undo), phase.next(undo), "a resume");
                    assert_eq!(to.next(undo), Ok(Step::Stay));
                }
            }
        }
        assert_eq!(Phase::Undone.next(false), Err(Refusal::Undone));
        assert_eq!(Phase::Undone.next(true), Ok(Step::Stay));
    }

    /// A settled phase names one state of the files; only a phase that
    /// recorded a start allows a mix.
    #[test]
    fn only_an_in_progress_phase_allows_a_mix_of_before_and_after() {
        use Phase::*;
        for (phase, files) in [
            (Prepared, Files::Before),
            (Applying, Files::Either),
            (Applied, Files::After),
            (Undoing, Files::Either),
            (Undone, Files::Before),
        ] {
            assert_eq!(phase.files(), files, "{phase}");
            assert_eq!(phase.in_progress(), files == Files::Either, "{phase}");
        }
    }

    /// The five words read and write as they always have, as a bare JSON
    /// string, and any other word is refused.
    #[test]
    fn the_five_words_round_trip_unchanged_and_an_unknown_word_is_refused() {
        use Phase::*;
        let words = [
            (Prepared, "prepared"),
            (Applying, "applying"),
            (Applied, "applied"),
            (Undoing, "undoing"),
            (Undone, "undone"),
        ];
        assert_eq!(words.map(|(phase, _)| phase), Phase::ALL);
        for (phase, word) in words {
            let json = format!("\"{word}\"");
            assert_eq!(serde_json::to_string(&phase).unwrap(), json);
            assert_eq!(serde_json::from_str::<Phase>(&json).unwrap(), phase);
            assert_eq!(serde_json::to_value(phase).unwrap(), word);
            assert_eq!(word.parse::<Phase>().unwrap(), phase);
            assert_eq!(phase.to_string(), word);
        }
        for unknown in ["paused", "Applied", "APPLIED", " applied", ""] {
            assert_eq!(
                unknown.parse::<Phase>(),
                Err(UnknownPhase(unknown.to_owned()))
            );
            let error = serde_json::from_value::<Phase>(serde_json::json!(unknown)).unwrap_err();
            assert!(
                error.to_string().contains(&format!(
                    "unreadable setup receipt: unknown phase `{unknown}`"
                )),
                "{error}"
            );
        }
        for other in ["null", "1", "{}", "[\"applied\"]"] {
            assert!(serde_json::from_str::<Phase>(other).is_err(), "{other}");
        }
    }
}
