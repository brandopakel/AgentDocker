//! Idempotent sends. A client that loses the daemon's answer to a `send`
//! cannot tell whether the message was queued; sending again would queue
//! it twice. A sender may give a `send` an idempotency key, and the daemon
//! answers a repeated key from one sender with the first answer instead of
//! queueing again. The ledger here is the pure part: bounded in entries
//! and in age, judged against a `now` the caller passes.
use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Duration, Utc};

/// How many answers the ledger keeps, over every sender.
pub const CAPACITY: usize = 4_096;

/// How long a key is remembered.
pub const RETENTION: Duration = Duration::hours(24);

/// The longest key.
pub const KEY_CHARS: usize = 128;

/// A key is one line of at most [`KEY_CHARS`] characters with no blanks
/// around it: a client's own token for one logical send, such as a UUID.
pub fn check_key(key: &str) -> Result<(), &'static str> {
    if key.is_empty() || key.trim().is_empty() {
        return Err("an idempotency key is not empty");
    }
    if key != key.trim() {
        return Err("an idempotency key has no leading or trailing blanks");
    }
    if key.chars().count() > KEY_CHARS || key.contains(['\0', '\n', '\r']) {
        return Err("an idempotency key is one line of at most 128 characters");
    }
    Ok(())
}

/// The answers given to keyed sends, by sender and key, oldest first.
///
/// `record` keeps the first answer for a key and ignores later ones; when
/// the ledger is full the oldest entry goes, and an entry older than
/// [`RETENTION`] goes on the next look or record. Two senders using the
/// same key are two entries.
#[derive(Clone, Debug)]
pub struct IdempotencyLedger<T> {
    order: VecDeque<(Key, DateTime<Utc>)>,
    answers: HashMap<Key, T>,
    capacity: usize,
    retention: Duration,
}

type Key = (String, String);

impl<T> Default for IdempotencyLedger<T> {
    fn default() -> Self {
        Self::new(CAPACITY, RETENTION)
    }
}

impl<T> IdempotencyLedger<T> {
    pub fn new(capacity: usize, retention: Duration) -> Self {
        Self {
            order: VecDeque::new(),
            answers: HashMap::new(),
            capacity,
            retention,
        }
    }

    /// The answer this sender was given for this key, if it is still kept.
    pub fn lookup(&mut self, sender: &str, key: &str, now: DateTime<Utc>) -> Option<&T> {
        self.expire(now);
        self.answers.get(&(sender.to_owned(), key.to_owned()))
    }

    /// Keep this answer for this sender's key, unless one is kept already.
    pub fn record(&mut self, sender: &str, key: &str, answer: T, now: DateTime<Utc>) {
        self.expire(now);
        let entry = (sender.to_owned(), key.to_owned());
        if self.answers.contains_key(&entry) {
            return;
        }
        self.order.push_back((entry.clone(), now));
        self.answers.insert(entry, answer);
        while self.order.len() > self.capacity {
            self.drop_oldest();
        }
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    fn expire(&mut self, now: DateTime<Utc>) {
        while self
            .order
            .front()
            .is_some_and(|(_, at)| now.signed_duration_since(*at) >= self.retention)
        {
            self.drop_oldest();
        }
    }

    fn drop_oldest(&mut self) {
        if let Some((entry, _)) = self.order.pop_front() {
            self.answers.remove(&entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + seconds, 0).unwrap()
    }

    #[test]
    fn keys_are_one_bounded_line_without_surrounding_blanks() {
        assert!(check_key("a").is_ok());
        assert!(check_key(&"k".repeat(KEY_CHARS)).is_ok());
        assert!(check_key("").is_err());
        assert!(check_key("   ").is_err());
        assert!(check_key(" padded").is_err());
        assert!(check_key("two\nlines").is_err());
        assert!(check_key(&"k".repeat(KEY_CHARS + 1)).is_err());
    }

    #[test]
    fn the_first_answer_is_kept_and_repeated() {
        let mut ledger = IdempotencyLedger::default();
        assert_eq!(ledger.lookup("a", "k", at(0)), None);
        ledger.record("a", "k", "first", at(0));
        ledger.record("a", "k", "second", at(1));
        assert_eq!(ledger.lookup("a", "k", at(2)), Some(&"first"));
        assert_eq!(ledger.len(), 1);
    }

    #[test]
    fn the_same_key_from_two_senders_is_two_entries() {
        let mut ledger = IdempotencyLedger::default();
        ledger.record("a", "k", "for a", at(0));
        ledger.record("b", "k", "for b", at(0));
        assert_eq!(ledger.len(), 2);
        assert_eq!(ledger.lookup("a", "k", at(1)), Some(&"for a"));
        assert_eq!(ledger.lookup("b", "k", at(1)), Some(&"for b"));
        assert_eq!(ledger.lookup("c", "k", at(1)), None);
    }

    #[test]
    fn a_full_ledger_forgets_its_oldest_entry() {
        let mut ledger = IdempotencyLedger::new(3, RETENTION);
        for (i, key) in ["k1", "k2", "k3", "k4"].iter().enumerate() {
            ledger.record("a", key, i, at(i as i64));
        }
        assert_eq!(ledger.len(), 3);
        assert_eq!(ledger.lookup("a", "k1", at(10)), None, "the oldest went");
        assert_eq!(ledger.lookup("a", "k2", at(10)), Some(&1));
        assert_eq!(ledger.lookup("a", "k4", at(10)), Some(&3));
    }

    #[test]
    fn an_entry_expires_after_the_retention() {
        let retention = RETENTION.num_seconds();
        let mut ledger = IdempotencyLedger::default();
        ledger.record("a", "old", "old", at(0));
        ledger.record("a", "new", "new", at(retention - 1));
        assert_eq!(ledger.lookup("a", "old", at(retention - 1)), Some(&"old"));
        assert_eq!(ledger.lookup("a", "old", at(retention)), None);
        assert_eq!(ledger.lookup("a", "new", at(retention)), Some(&"new"));
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger.lookup("a", "new", at(2 * retention)), None);
        assert!(ledger.is_empty());
    }

    #[test]
    fn an_expired_key_may_be_recorded_again() {
        let retention = RETENTION.num_seconds();
        let mut ledger = IdempotencyLedger::default();
        ledger.record("a", "k", "first", at(0));
        ledger.record("a", "k", "again", at(retention));
        assert_eq!(ledger.lookup("a", "k", at(retention)), Some(&"again"));
    }
}
