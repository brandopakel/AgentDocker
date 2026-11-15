//! Something the window reads from the daemon, and where its latest read
//! stands.
//!
//! These were `Option<T>`, and one `None` stood for four things: never
//! asked for, being read, read and found empty, and could not be read. A
//! page drew the same blank for all of them, so a board that failed to
//! load looked like an empty board, and a failure, where it was said at
//! all, was a passing line at the window's foot.
//!
//! A [`Remote`] keeps them apart. It holds the last value read, which
//! stays on view while a newer read is out and when one fails, and
//! beside it where the latest ask stands. Each ask has a number
//! ([`Ask`]) that goes out with the request and comes back with the
//! answer: an answer to an ask that is no longer the latest is not
//! taken, so a late one cannot put back an older value, and one about
//! another subject (the card or session that was open before) cannot
//! land on this one.

/// One ask of a [`Remote`]: its number, which the answer brings back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ask(u64);

/// Where the latest ask stands.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Read {
    /// Nothing has been asked, of this subject at least.
    Unasked,
    /// An ask is out, and this is the one whose answer is taken.
    Reading(Ask),
    /// That ask was answered.
    Done,
    /// The latest ask could not be answered, and why.
    Failed(String),
}

/// A value read from the daemon: the last one read, and where the
/// latest read stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Remote<T> {
    last: Option<T>,
    read: Read,
    /// How many asks there have been. It never goes back, so no two
    /// asks share a number, a forgotten subject's included.
    asks: u64,
}

impl<T> Default for Remote<T> {
    fn default() -> Self {
        Remote {
            last: None,
            read: Read::Unasked,
            asks: 0,
        }
    }
}

/// What a view draws of a [`Remote`]: four cases, each to be drawn.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Seen<'a, T> {
    /// Never asked for.
    Unasked,
    /// Being read; `last` is what was read before, to go on showing.
    Reading { last: Option<&'a T> },
    /// Read: the value, which may be an empty one.
    Ready(&'a T),
    /// Could not be read, and why; `last` is what was read before.
    Failed { error: &'a str, last: Option<&'a T> },
}

impl<'a, T> Seen<'a, T> {
    /// The value to show, whichever case it comes with.
    pub fn value(&self) -> Option<&'a T> {
        match self {
            Seen::Unasked => None,
            Seen::Reading { last } | Seen::Failed { last, .. } => *last,
            Seen::Ready(value) => Some(value),
        }
    }

    /// Why the latest read failed, when it did.
    pub fn error(&self) -> Option<&'a str> {
        match self {
            Seen::Failed { error, .. } => Some(error),
            _ => None,
        }
    }

    /// Being read with nothing yet to show: the one case that says
    /// "Reading…". A read behind a value on view says nothing.
    pub fn first_read(&self) -> bool {
        matches!(self, Seen::Reading { last: None })
    }
}

impl<T> Remote<T> {
    /// A read begins. The number goes out with the request, and its
    /// answer is the one now awaited.
    pub fn ask(&mut self) -> Ask {
        self.asks += 1;
        let ask = Ask(self.asks);
        self.read = Read::Reading(ask);
        ask
    }

    /// The request just asked was folded into one already waiting in
    /// the queue, which will answer for both. `waiting` is the ask that
    /// one carries, as the queue kept it: its answer is the one awaited.
    /// It is given, not worked out: the asks between the two may have
    /// been about other subjects.
    pub fn merged(&mut self, waiting: Ask) {
        if matches!(self.read, Read::Reading(_)) {
            self.read = Read::Reading(waiting);
        }
    }

    /// An answer. Taken only when it is to the ask that is out; returns
    /// whether it was. A value replaces the last one; a failure is kept
    /// beside it.
    pub fn answer(&mut self, ask: Ask, result: Result<T, String>) -> bool {
        if self.read != Read::Reading(ask) {
            return false;
        }
        match result {
            Ok(value) => {
                self.last = Some(value);
                self.read = Read::Done;
            }
            Err(error) => self.read = Read::Failed(error),
        }
        true
    }

    /// Another subject: what was read is not about it, and no answer
    /// is awaited, so one still on its way is let go.
    pub fn forget(&mut self) {
        self.last = None;
        self.read = Read::Unasked;
    }

    /// What a view draws.
    pub fn seen(&self) -> Seen<'_, T> {
        let last = self.last.as_ref();
        match &self.read {
            Read::Unasked => Seen::Unasked,
            Read::Reading(_) => Seen::Reading { last },
            Read::Done => match last {
                Some(value) => Seen::Ready(value),
                None => Seen::Unasked,
            },
            Read::Failed(error) => Seen::Failed { error, last },
        }
    }

    /// The last value read, whatever the latest read came to.
    pub fn value(&self) -> Option<&T> {
        self.last.as_ref()
    }

    /// A value as though it had just been read: what a test starts from.
    #[cfg(test)]
    pub fn ready(value: T) -> Self {
        Remote {
            last: Some(value),
            read: Read::Done,
            asks: 0,
        }
    }

    /// The value, for a test to change what was read.
    #[cfg(test)]
    pub fn value_mut(&mut self) -> Option<&mut T> {
        self.last.as_mut()
    }

    /// The ask that is out (else the last one made), for a test to
    /// answer what the window asked.
    #[cfg(test)]
    pub fn latest(&self) -> Ask {
        match self.read {
            Read::Reading(ask) => ask,
            _ => Ask(self.asks),
        }
    }
}

/// A read about one subject at a time: a session's changes, a session's
/// log. Asking about another subject forgets the first, and what is
/// shown of a subject is only ever what was read about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct About<K, T> {
    subject: Option<K>,
    read: Remote<T>,
}

impl<K, T> Default for About<K, T> {
    fn default() -> Self {
        About {
            subject: None,
            read: Remote::default(),
        }
    }
}

impl<K: PartialEq, T> About<K, T> {
    /// A read about `subject` begins. What was read about another is
    /// forgotten; what was read about this one stays on view meanwhile.
    pub fn ask(&mut self, subject: K) -> Ask {
        if self.subject.as_ref() != Some(&subject) {
            self.read.forget();
            self.subject = Some(subject);
        }
        self.read.ask()
    }

    /// The request just asked was folded into one already waiting,
    /// which carries `waiting` ([`Remote::merged`]).
    pub fn merged(&mut self, waiting: Ask) {
        self.read.merged(waiting);
    }

    /// An answer about `subject`. Taken only when that is the subject
    /// now and the ask is the latest ([`Remote::answer`]).
    pub fn answer(&mut self, subject: &K, ask: Ask, result: Result<T, String>) -> bool {
        self.subject.as_ref() == Some(subject) && self.read.answer(ask, result)
    }

    /// What a view draws of `subject`: never asked, when what there is
    /// is about another.
    pub fn of<Q>(&self, subject: &Q) -> Seen<'_, T>
    where
        K: std::borrow::Borrow<Q>,
        Q: PartialEq + ?Sized,
    {
        match &self.subject {
            Some(shown) if shown.borrow() == subject => self.read.seen(),
            _ => Seen::Unasked,
        }
    }

    /// What was read about `subject`, as though just now: what a test
    /// starts from.
    #[cfg(test)]
    pub fn ready(subject: K, value: T) -> Self {
        About {
            subject: Some(subject),
            read: Remote::ready(value),
        }
    }

    /// The value, for a test to change what was read.
    #[cfg(test)]
    pub fn value_mut(&mut self) -> Option<&mut T> {
        self.read.value_mut()
    }

    /// The latest ask, for a test to answer what the window asked.
    #[cfg(test)]
    pub fn latest(&self) -> Ask {
        self.read.latest()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What is shown of a subject is only what was read about it, and
    /// asking about another forgets the first.
    #[test]
    fn a_read_about_one_subject_is_not_shown_of_another() {
        let mut changes: About<String, &str> = About::default();
        assert_eq!(changes.of("agent-1"), Seen::Unasked);
        let first = changes.ask("agent-1".to_owned());
        assert_eq!(changes.of("agent-1"), Seen::Reading { last: None });
        assert_eq!(changes.of("agent-2"), Seen::Unasked, "nothing asked of it");
        assert!(changes.answer(&"agent-1".to_owned(), first, Ok("one's diff")));
        // Read again: the last value stays on view, and a failure is
        // beside it.
        let again = changes.ask("agent-1".to_owned());
        assert_eq!(
            changes.of("agent-1"),
            Seen::Reading {
                last: Some(&"one's diff")
            }
        );
        assert!(changes.answer(&"agent-1".to_owned(), again, Err("no git".into())));
        assert_eq!(
            changes.of("agent-1"),
            Seen::Failed {
                error: "no git",
                last: Some(&"one's diff")
            }
        );

        // Another session: the first one's diff is not shown as its own,
        // and an answer still on its way about the first is let go.
        let late = changes.ask("agent-1".to_owned());
        let other = changes.ask("agent-2".to_owned());
        assert_eq!(changes.of("agent-2"), Seen::Reading { last: None });
        assert_eq!(changes.of("agent-1"), Seen::Unasked);
        assert!(!changes.answer(&"agent-1".to_owned(), late, Ok("one's, late")));
        assert!(!changes.answer(&"agent-2".to_owned(), late, Ok("one's, late")));
        assert!(changes.answer(&"agent-2".to_owned(), other, Ok("two's diff")));
        assert_eq!(changes.of("agent-2"), Seen::Ready(&"two's diff"));
    }

    /// The four things one `None` stood for are four cases.
    #[test]
    fn never_asked_reading_empty_and_failed_are_told_apart() {
        let mut list: Remote<Vec<u32>> = Remote::default();
        assert_eq!(list.seen(), Seen::Unasked);

        let first = list.ask();
        assert_eq!(list.seen(), Seen::Reading { last: None });
        assert!(list.seen().first_read());

        assert!(list.answer(first, Ok(Vec::new())));
        assert_eq!(list.seen(), Seen::Ready(&Vec::new()), "read, and empty");
        assert!(!list.seen().first_read());

        let again = list.ask();
        assert_eq!(
            list.seen(),
            Seen::Reading {
                last: Some(&Vec::new())
            },
            "a read behind a value keeps it on view"
        );
        assert!(!list.seen().first_read(), "and does not say Reading…");
        assert!(list.answer(again, Err("storage unavailable".into())));
        assert_eq!(
            list.seen(),
            Seen::Failed {
                error: "storage unavailable",
                last: Some(&Vec::new())
            },
            "a failure is beside the last value, which stays"
        );
        assert_eq!(list.seen().error(), Some("storage unavailable"));
        assert_eq!(list.value(), Some(&Vec::new()));

        // A first read that fails has nothing beside it.
        let mut never: Remote<Vec<u32>> = Remote::default();
        let ask = never.ask();
        never.answer(ask, Err("no such project".into()));
        assert_eq!(
            never.seen(),
            Seen::Failed {
                error: "no such project",
                last: None
            }
        );
    }

    /// Only the latest ask's answer is taken: a late one cannot put back
    /// an older value, and a later success clears an earlier failure.
    #[test]
    fn only_the_latest_asks_answer_is_taken() {
        let mut value: Remote<&str> = Remote::default();
        let first = value.ask();
        let second = value.ask();
        assert!(!value.answer(first, Ok("older")), "superseded");
        assert_eq!(value.seen(), Seen::Reading { last: None }, "still out");
        assert!(value.answer(second, Ok("newer")));
        assert!(!value.answer(first, Ok("older")), "nor after the newer");
        assert!(!value.answer(second, Err("twice".into())), "nor twice over");
        let third = value.ask();
        assert!(value.answer(third, Err("gone".into())));
        let fourth = value.ask();
        assert!(value.answer(fourth, Ok("back")));
        assert_eq!(value.seen(), Seen::Ready(&"back"));
    }

    /// Another subject forgets what was read, and what is still on its
    /// way about the first does not land on the second.
    #[test]
    fn an_answer_about_another_subject_does_not_land_on_this_one() {
        let mut card: Remote<&str> = Remote::default();
        let about_one = card.ask();
        card.forget();
        assert_eq!(card.seen(), Seen::Unasked, "nothing asked of the second");
        assert!(!card.answer(about_one, Ok("the first card")));
        assert_eq!(card.seen(), Seen::Unasked);
        let about_two = card.ask();
        assert_ne!(about_one, about_two);
        assert!(!card.answer(about_one, Ok("the first card")));
        assert!(card.answer(about_two, Ok("the second card")));
        assert_eq!(card.value(), Some(&"the second card"));
        // Forgotten with a value on view: gone, not shown as the next's.
        card.forget();
        assert_eq!(card.value(), None);
        // And an answer to no ask at all is not taken.
        let mut unasked: Remote<&str> = Remote::default();
        let none = unasked.latest();
        assert!(!unasked.answer(none, Ok("from nowhere")));
        assert_eq!(unasked.seen(), Seen::Unasked);
    }

    /// A request folded into one already waiting is answered by that
    /// one, whose ask the queue gives back: the read is not left out for
    /// an answer that will never come. The ask is not the one before the
    /// folded one: asks about other subjects may lie between them.
    #[test]
    fn a_request_folded_into_one_waiting_is_answered_by_it() {
        let mut review: Remote<&str> = Remote::default();
        let waiting = review.ask();
        let _folded = review.ask();
        review.merged(waiting);
        assert_eq!(review.seen(), Seen::Reading { last: None }, "still out");
        assert!(review.answer(waiting, Ok("the diff")));
        assert_eq!(review.seen(), Seen::Ready(&"the diff"));

        // The same with other subjects asked about in between, as a
        // keyed read does it: A, B, A, the second A folded into the first.
        let mut changes: About<&str, &str> = About::default();
        let first_a = changes.ask("a");
        let about_b = changes.ask("b");
        let _second_a = changes.ask("a");
        changes.merged(first_a);
        assert!(
            !changes.answer(&"b", about_b, Ok("b's diff")),
            "not on view"
        );
        assert!(changes.answer(&"a", first_a, Ok("a's diff")));
        assert_eq!(changes.of("a"), Seen::Ready(&"a's diff"));

        // Folded when nothing is out (the answer came first): no ask is
        // put back out.
        let mut done: Remote<&str> = Remote::default();
        let ask = done.ask();
        done.answer(ask, Ok("read"));
        done.merged(ask);
        assert_eq!(done.seen(), Seen::Ready(&"read"));
    }
}
