//! Idempotency keys: what makes a retried send the same send.
//!
//! A sender that cannot tell whether its send landed (a connection cut
//! after the request was written, a connector whose HTTP client retries, a
//! model that calls a tool again after a timeout) sends again with the same
//! key, and gets the first send's answer instead of a second message.
//!
//! The ledger is pure: the caller passes `now`. It is bounded in age
//! ([`WINDOW_HOURS`]) and in size ([`CAPACITY`], oldest forgotten first), so
//! a sender cannot grow the daemon's memory by choosing new keys.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Duration, Utc};

/// How long a key is remembered.
pub const WINDOW_HOURS: i64 = 24;
/// How many keys are remembered across all senders.
pub const CAPACITY: usize = 4096;
/// The longest key, in bytes.
pub const KEY_MAX: usize = 128;

/// A key is 1 to [`KEY_MAX`] printable ASCII characters, no spaces: what a
/// UUID, a hash or a `run-42/step-3` is, and nothing a log could misprint.
pub fn check(key: &str) -> Result<(), String> {
    if key.is_empty() || key.len() > KEY_MAX || !key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(format!(
            "an idempotency key is 1 to {KEY_MAX} printable ASCII characters without spaces"
        ));
    }
    Ok(())
}

/// The answers to keyed requests, by sender and key.
#[derive(Debug)]
pub struct Ledger<V> {
    entries: HashMap<(String, String), (DateTime<Utc>, V)>,
    /// Insertion order, for forgetting the oldest.
    order: VecDeque<(String, String)>,
}

impl<V> Default for Ledger<V> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }
}

impl<V: Clone> Ledger<V> {
    /// The answer recorded for this sender's key, when it is still
    /// remembered at `now`.
    pub fn get(&mut self, sender: &str, key: &str, now: DateTime<Utc>) -> Option<V> {
        self.forget_expired(now);
        self.entries
            .get(&(sender.to_owned(), key.to_owned()))
            .map(|(_, value)| value.clone())
    }

    /// Remember the answer to this sender's key, and say whether it was
    /// new. A key already remembered keeps its first answer: the first
    /// send is the one that happened.
    pub fn record(&mut self, sender: &str, key: &str, now: DateTime<Utc>, value: V) -> bool {
        self.forget_expired(now);
        let id = (sender.to_owned(), key.to_owned());
        if self.entries.contains_key(&id) {
            return false;
        }
        while self.order.len() >= CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
        self.order.push_back(id.clone());
        self.entries.insert(id, (now, value));
        true
    }

    /// Replace the answer a key holds, keeping when it was recorded: a
    /// reservation becoming the answer it reserved.
    pub fn settle(&mut self, sender: &str, key: &str, value: V) {
        if let Some((_, held)) = self.entries.get_mut(&(sender.to_owned(), key.to_owned())) {
            *held = value;
        }
    }

    /// Forget a key: a reservation whose request was refused, so that a
    /// retry is a fresh attempt.
    pub fn forget(&mut self, sender: &str, key: &str) {
        let id = (sender.to_owned(), key.to_owned());
        if self.entries.remove(&id).is_some() {
            self.order.retain(|held| *held != id);
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn forget_expired(&mut self, now: DateTime<Utc>) {
        let cutoff = now - Duration::hours(WINDOW_HOURS);
        while let Some(oldest) = self.order.front() {
            match self.entries.get(oldest) {
                Some((at, _)) if *at > cutoff => break,
                _ => {
                    let oldest = self.order.pop_front().expect("front exists");
                    self.entries.remove(&oldest);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn at(hours: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(1_800_000_000, 0).unwrap() + Duration::hours(hours)
    }

    #[test]
    fn a_key_returns_the_first_answer_per_sender_within_the_window() {
        let mut ledger = Ledger::default();
        assert_eq!(ledger.get("a", "k", at(0)), None);
        ledger.record("a", "k", at(0), 1);
        ledger.record("a", "k", at(1), 2);
        assert_eq!(
            ledger.get("a", "k", at(2)),
            Some(1),
            "the first send is the one"
        );
        assert_eq!(ledger.get("b", "k", at(2)), None, "keys are per sender");
        assert_eq!(ledger.get("a", "k", at(WINDOW_HOURS - 1)), Some(1));
        assert_eq!(
            ledger.get("a", "k", at(WINDOW_HOURS + 1)),
            None,
            "forgotten"
        );
        assert!(ledger.is_empty());
    }

    /// A reservation is answered by settling it, or dropped so a retry
    /// starts again.
    #[test]
    fn reservations_settle_or_are_forgotten() {
        let mut ledger: Ledger<Option<u8>> = Ledger::default();
        assert!(ledger.record("a", "k", at(0), None));
        assert!(!ledger.record("a", "k", at(0), None), "taken");
        assert_eq!(ledger.get("a", "k", at(0)), Some(None), "in progress");
        ledger.settle("a", "k", Some(7));
        assert_eq!(ledger.get("a", "k", at(1)), Some(Some(7)));
        assert!(ledger.record("a", "j", at(1), None));
        ledger.forget("a", "j");
        assert_eq!(ledger.get("a", "j", at(1)), None);
        assert_eq!(ledger.len(), 1);
        ledger.settle("a", "nope", Some(1));
        assert_eq!(
            ledger.get("a", "nope", at(1)),
            None,
            "settling records nothing"
        );
    }

    #[test]
    fn keys_are_checked_for_shape() {
        assert!(check("3f2c9a1e-0b7d-4c55-9a3e-2f1b6d8c4e70").is_ok());
        assert!(check("run-42/step-3").is_ok());
        for bad in [
            "",
            "has space",
            "tab\t",
            "ünïcode",
            &"x".repeat(KEY_MAX + 1),
        ] {
            assert!(check(bad).is_err(), "{bad:?}");
        }
        assert!(check(&"x".repeat(KEY_MAX)).is_ok());
    }

    proptest! {
        /// Whatever is recorded, the ledger never holds more than its
        /// capacity, every key it answers was recorded with that answer
        /// first, and nothing older than the window is answered.
        #[test]
        fn the_ledger_stays_bounded_and_answers_only_first_recordings(
            ops in proptest::collection::vec((0u8..4, 0u16..6000, 0i64..60), 1..400)
        ) {
            let mut ledger = Ledger::default();
            // The model: when each key was last recorded afresh, and with what.
            let mut model: HashMap<(String, String), (i64, u64)> = HashMap::new();
            let mut clock = 0;
            for (n, (sender, key, step)) in ops.into_iter().enumerate() {
                clock += step % 3; // time only moves forward
                let id = (sender.to_string(), key.to_string());
                let value = n as u64; // a different answer every time
                match ledger.get(&id.0, &id.1, at(clock)) {
                    Some(answer) => {
                        let (when, first) = model[&id];
                        prop_assert_eq!(answer, first, "the first answer, not a later one");
                        prop_assert!(clock - when < WINDOW_HOURS);
                    }
                    None => {
                        model.insert(id.clone(), (clock, value));
                    }
                }
                ledger.record(&id.0, &id.1, at(clock), value);
                prop_assert!(ledger.len() <= CAPACITY);
            }
        }
    }
}
