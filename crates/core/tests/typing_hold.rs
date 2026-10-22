//! The typing guard as a deliverer drives it: the person types, ends
//! lines and pauses; the deliverer looks at its queue now and then, holds
//! a peer's message while the rule says wait, and shows the hold when the
//! rule says to announce it.
use agentdocker_core::typing::{Hold, QUIET, announce, decide, plausible};
use chrono::{DateTime, Duration, Utc};
use proptest::prelude::*;

#[derive(Clone, Copy, Debug)]
enum Step {
    /// A keystroke of an unfinished line.
    Key,
    /// Enter: the line is the person's and ends their draft.
    Enter,
    /// The deliverer looks at its queue.
    Poll,
}

fn step() -> impl Strategy<Value = (Step, i64)> {
    (
        prop_oneof![
            4 => Just(Step::Key),
            1 => Just(Step::Enter),
            4 => Just(Step::Poll),
        ],
        // Gaps between events, from keystroke-fast to a long pause.
        prop_oneof![0i64..400, 400i64..4_000],
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]
    #[test]
    fn a_held_message_goes_once_and_only_after_a_quiet_window(
        steps in prop::collection::vec(step(), 1..200),
    ) {
        let every = Duration::milliseconds(500);
        let base = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let mut now = base;
        let mut typed_at: Option<DateTime<Utc>> = None;
        let mut delivered: Option<DateTime<Utc>> = None;
        let mut announced: Option<DateTime<Utc>> = None;
        let mut announcements = Vec::new();
        let mut keys = 0usize;
        for (step, gap) in steps {
            now += Duration::milliseconds(gap);
            match step {
                Step::Key => {
                    typed_at = Some(now);
                    keys += 1;
                }
                Step::Enter => typed_at = None,
                Step::Poll if delivered.is_some() => {}
                Step::Poll => match decide(typed_at, now) {
                    Hold::Deliver => {
                        // Never into a line typed within the window.
                        prop_assert!(typed_at.is_none_or(|at| now - at >= QUIET));
                        delivered = Some(now);
                    }
                    Hold::Until(until) => {
                        let at = typed_at.expect("only typing holds");
                        prop_assert_eq!(until, at + QUIET);
                        prop_assert!(now < until && until <= now + QUIET);
                        // The daemon believes every hold the rule makes.
                        prop_assert!(plausible(until, now));
                        if announce(announced, until, now, every) {
                            if let Some(previous) = announced {
                                prop_assert!(until > previous, "an announcement says more");
                            }
                            announced = Some(until);
                            announcements.push(until);
                        }
                        // What is shown covers this moment.
                        prop_assert!(announced.is_some_and(|shown| shown > now));
                    }
                },
            }
        }
        // At most one announcement a keystroke, never one a look.
        prop_assert!(announcements.len() <= keys);
        // Liveness: a look once the terminal has been quiet a window
        // delivers what waited, whatever came before.
        if delivered.is_none() {
            let quiet = typed_at.map_or(now, |at| at + QUIET).max(now);
            prop_assert_eq!(decide(typed_at, quiet), Hold::Deliver);
        }
    }

    #[test]
    fn a_wait_is_never_longer_than_one_window_and_ends_in_delivery(
        age in 0i64..10_000,
        later in 0i64..10_000,
    ) {
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let typed_at = now - Duration::milliseconds(age);
        match decide(Some(typed_at), now) {
            Hold::Deliver => prop_assert!(now - typed_at >= QUIET),
            Hold::Until(until) => {
                prop_assert!(now < until && until <= now + QUIET);
                prop_assert_eq!(decide(Some(typed_at), until), Hold::Deliver);
                // Deciding again later never turns a delivery back into a wait.
                prop_assert_eq!(
                    decide(Some(typed_at), until + Duration::milliseconds(later)),
                    Hold::Deliver
                );
            }
        }
        // A keystroke stamped in the future waits one window from now.
        let ahead = now + Duration::milliseconds(age + 1);
        prop_assert_eq!(decide(Some(ahead), now), Hold::Until(now + QUIET));
    }
}
