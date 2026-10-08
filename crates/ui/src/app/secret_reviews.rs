//! Only RAM holds edits to temporary questions. Ordinary draft persistence,
//! conversation messages and automatic submission retries never receive them.
use agentdocker_core::secret::{SecretAnswers, SecretReview, SecretText};
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct Draft {
    pub values: BTreeMap<String, SecretText>,
    pub retention_acknowledged: bool,
}

#[derive(Default)]
pub(super) struct State {
    pub reviews: Vec<SecretReview>,
    pub drafts: BTreeMap<String, Draft>,
    // Submission uncertainty survives disconnect within this window, but not
    // process restart. The daemon refuses duplicates and never reuses an ID.
    attempted: BTreeMap<String, DateTime<Utc>>,
}

impl State {
    pub fn disconnect(&mut self) {
        self.reviews.clear();
        self.drafts.clear();
    }

    pub fn expire(&mut self, now: DateTime<Utc>) {
        self.reviews.retain(|r| r.expires_at > now);
        self.drafts
            .retain(|id, _| self.reviews.iter().any(|r| &r.id == id));
        self.attempted.retain(|_, expires| *expires > now);
    }

    pub fn refresh(&mut self, reviews: Vec<SecretReview>, now: DateTime<Utc>) {
        self.expire(now);
        // A malformed response cannot grow retained drafts or change a field
        // under an existing edit. Close this view and await another fresh list.
        if reviews.len() > 32 || reviews.iter().any(|r| !r.request.valid()) {
            self.disconnect();
            return;
        }
        self.drafts.retain(|id, _| {
            reviews.iter().any(|r| {
                &r.id == id && r.expires_at > now && self.reviews.iter().any(|old| old == r)
            })
        });
        self.reviews = reviews.into_iter().filter(|r| r.expires_at > now).collect();
    }

    pub fn attempted(&self, id: &str) -> bool {
        self.attempted.contains_key(id)
    }

    pub fn edit(&mut self, id: &str, field: &str, value: SecretText, now: DateTime<Utc>) -> bool {
        self.expire(now);
        if self.attempted(id)
            || !self
                .reviews
                .iter()
                .any(|r| r.id == id && r.request.fields.iter().any(|f| f.id == field))
        {
            return false;
        }
        let draft = self.drafts.entry(id.into()).or_default();
        let other_bytes = draft
            .values
            .iter()
            .filter(|(key, _)| key.as_str() != field)
            .map(|(_, v)| v.expose().len())
            .sum::<usize>();
        if other_bytes + value.expose().len() > agentdocker_core::secret::MAX_ANSWER_BYTES {
            return false;
        }
        draft.values.insert(field.into(), value);
        true
    }

    pub fn acknowledge(&mut self, id: &str, value: bool, now: DateTime<Utc>) {
        self.expire(now);
        if !self.attempted(id) && self.reviews.iter().any(|r| r.id == id) {
            self.drafts
                .entry(id.into())
                .or_default()
                .retention_acknowledged = value;
        }
    }

    pub fn ready(&self, id: &str, now: DateTime<Utc>) -> bool {
        let Some(review) = self
            .reviews
            .iter()
            .find(|r| r.id == id && r.expires_at > now)
        else {
            return false;
        };
        let Some(draft) = self.drafts.get(id) else {
            return false;
        };
        !self.attempted(id)
            && self.attempted.len() < 128
            && draft.retention_acknowledged
            && draft.values.len() == review.request.fields.len()
            && review.request.fields.iter().all(|f| {
                draft
                    .values
                    .get(&f.id)
                    .is_some_and(|v| !v.expose().is_empty())
            })
    }

    pub fn take(&mut self, id: &str, now: DateTime<Utc>) -> Option<SecretAnswers> {
        self.expire(now);
        if !self.ready(id, now) {
            return None;
        }
        let expires = self.reviews.iter().find(|r| r.id == id)?.expires_at;
        let draft = self.drafts.remove(id)?;
        let answers = SecretAnswers::new(draft.values).ok()?;
        self.attempted.insert(id.into(), expires);
        Some(answers)
    }

    pub fn cancel(&mut self, id: &str, now: DateTime<Utc>) -> bool {
        self.expire(now);
        let Some(review) = self.reviews.iter().find(|r| r.id == id) else {
            return false;
        };
        if self.attempted.len() >= 128 && !self.attempted(id) {
            return false;
        }
        self.drafts.remove(id);
        self.attempted.insert(id.into(), review.expires_at);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{
        ProcessIdentity,
        secret::{SecretField, SecretReviewSpec},
    };
    fn review(now: DateTime<Utc>) -> SecretReview {
        SecretReview {
            id: "route".into(),
            agent: "owner".into(),
            recipient: "human".into(),
            owner: ProcessIdentity {
                pid: 1,
                started_at: now,
            },
            expires_at: now + chrono::Duration::seconds(300),
            request: SecretReviewSpec {
                thread: "thread".into(),
                turn: "turn".into(),
                fields: vec![SecretField {
                    id: "value".into(),
                    question: "Temporary?".into(),
                    is_secret: true,
                }],
            },
        }
    }
    #[test]
    fn secret_draft_requires_consent_is_taken_once_and_is_never_restored_after_disconnect() {
        let now = Utc::now();
        let mut s = State::default();
        let r = review(now);
        s.refresh(vec![r.clone()], now);
        assert!(s.edit(
            "route",
            "value",
            SecretText::new("invented-only".into()).unwrap(),
            now
        ));
        assert!(!s.ready("route", now));
        s.acknowledge("route", true, now);
        assert!(s.ready("route", now));
        let answer = s.take("route", now).unwrap();
        assert!(!format!("{answer:?}").contains("invented-only"));
        assert!(s.drafts.is_empty());
        assert!(s.take("route", now).is_none());
        s.disconnect();
        s.refresh(vec![r], now);
        assert!(s.attempted("route"));
        assert!(!s.edit(
            "route",
            "value",
            SecretText::new("retry".into()).unwrap(),
            now
        ));
    }
    #[test]
    fn secret_drafts_clear_on_expiry_cancellation_route_change_and_disconnect() {
        let now = Utc::now();
        for cause in 0..4 {
            let mut s = State::default();
            let mut r = review(now);
            s.refresh(vec![r.clone()], now);
            s.edit(
                "route",
                "value",
                SecretText::new("temporary".into()).unwrap(),
                now,
            );
            match cause {
                0 => s.expire(now + chrono::Duration::seconds(301)),
                1 => {
                    assert!(s.cancel("route", now));
                }
                2 => {
                    r.request.turn = "replacement".into();
                    s.refresh(vec![r], now);
                }
                _ => s.disconnect(),
            }
            assert!(s.drafts.is_empty());
            assert!(!s.ready("route", now));
        }
    }
}
