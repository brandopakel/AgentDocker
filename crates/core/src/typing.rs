//! Holding delivered input while the person types in the same terminal.
//!
//! Where AgentDocker hands another party's message to a session whose
//! terminal the person is typing in, a delivery in the middle of their
//! line starts a turn whose output runs into their draft. The rule: input
//! from anyone but the person waits while their last keystroke there is
//! more recent than [`QUIET`], and goes once the terminal has been quiet
//! that long. Nothing is dropped: a held message stays at the head of its
//! queue and is delivered by the same path, once.
//!
//! Whoever reads the keystrokes keeps the clock; this module only decides.

use chrono::{DateTime, Duration, Utc};

/// How long a terminal must be quiet before held input goes into it.
/// Long enough to span the pause between words of a line being typed,
/// short enough that a message is not noticeably late once the person has
/// stopped.
pub const QUIET: Duration = Duration::seconds(3);

/// What to do with input waiting for a terminal now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    /// Nothing typed within [`QUIET`]: deliver it.
    Deliver,
    /// The person typed within [`QUIET`]: wait, and decide again at this
    /// time unless they type again first.
    Until(DateTime<Utc>),
}

/// Deliver now, or wait until when. `typed_at` is the last keystroke of a
/// line the person has not yet submitted (`None` when there is none: a
/// line they submitted is theirs and holds nothing). A keystroke stamped
/// after `now` counts as `now`, so no decision waits longer than one quiet
/// window; a deliverer measures a keystroke's age on a monotonic clock, so
/// that a wall clock stepping back cannot stamp it in the future.
pub fn decide(typed_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Hold {
    let Some(typed_at) = typed_at else {
        return Hold::Deliver;
    };
    let until = typed_at.min(now) + QUIET;
    if now < until {
        Hold::Until(until)
    } else {
        Hold::Deliver
    }
}

/// Whether a hold lasting `until` is worth announcing, given the one last
/// announced and how often the deliverer decides again (`every`): when it
/// begins, and again only when the announced one would lapse before the
/// next decision while the person types on. One announcement a window,
/// never one a keystroke.
pub fn announce(
    announced: Option<DateTime<Utc>>,
    until: DateTime<Utc>,
    now: DateTime<Utc>,
    every: Duration,
) -> bool {
    match announced {
        None => true,
        Some(announced) => until > announced && announced <= now + every,
    }
}

/// Whether a reported hold is one this rule could have produced: it ends
/// after it was observed and no more than one quiet window later.
pub fn plausible(until: DateTime<Utc>, observed_at: DateTime<Utc>) -> bool {
    until > observed_at && until <= observed_at + QUIET
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + seconds, 0).unwrap()
    }

    #[test]
    fn nothing_typed_delivers_at_once() {
        assert_eq!(decide(None, at(0)), Hold::Deliver);
    }

    #[test]
    fn a_recent_keystroke_holds_until_the_window_has_passed() {
        assert_eq!(decide(Some(at(0)), at(0)), Hold::Until(at(3)));
        assert_eq!(decide(Some(at(0)), at(2)), Hold::Until(at(3)));
        assert_eq!(decide(Some(at(0)), at(3)), Hold::Deliver);
        assert_eq!(decide(Some(at(0)), at(60)), Hold::Deliver);
        // Typing on moves the end of the wait with it.
        assert_eq!(decide(Some(at(2)), at(2)), Hold::Until(at(5)));
    }

    #[test]
    fn a_keystroke_stamped_in_the_future_waits_one_window_from_the_decision() {
        assert_eq!(decide(Some(at(3_600)), at(0)), Hold::Until(at(3)));
    }

    #[test]
    fn a_hold_is_announced_when_it_begins_and_once_more_a_window() {
        let every = Duration::milliseconds(500);
        assert!(announce(None, at(3), at(0), every));
        // Typing on within the window it announced: nothing new to say.
        assert!(!announce(Some(at(3)), at(4), at(1), every));
        // The announced window is about to lapse and the person still types.
        assert!(announce(
            Some(at(3)),
            at(5),
            at(3) - Duration::milliseconds(400),
            every
        ));
        // The person stopped: the announced hold lapses by itself.
        assert!(!announce(Some(at(3)), at(3), at(3), every));
    }

    #[test]
    fn a_reported_hold_must_end_within_one_window_of_its_observation() {
        assert!(plausible(at(3), at(0)));
        assert!(plausible(at(1), at(0)));
        assert!(!plausible(at(0), at(0)), "already over");
        assert!(!plausible(at(4), at(0)), "longer than a keystroke can hold");
    }
}
