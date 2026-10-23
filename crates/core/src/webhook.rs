//! A webhook sink's delivery log: one line for every attempt to post one
//! event to one sink, with what came of it and when it will be tried
//! again. A line carries the sink's configured name, the event's sequence
//! and kind, and the receiver's status code or a word for why there was
//! none — never the address, the secret, the body sent or anything the
//! receiver said beyond its status code.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Attempts kept for one sink; past that the oldest go.
pub const LOG_PER_WEBHOOK: usize = 200;
/// Attempts kept across every sink, including ones no longer configured;
/// past that the oldest go, whichever sink they were for.
pub const LOG_TOTAL: usize = 2_000;
/// What a `webhook_deliveries` request answers when it names no limit.
pub const LOG_LIMIT: usize = 50;

/// What one attempt came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptResult {
    /// The receiver answered 2xx: the delivery is done.
    Delivered,
    /// A 429, a 5xx or the network: tried again at `next_retry`.
    Retrying,
    /// No attempt follows: the receiver refused it (a 4xx other than
    /// 429), or the last retry failed too.
    GaveUp,
}

impl AttemptResult {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::Retrying => "retrying",
            Self::GaveUp => "gave_up",
        }
    }
}

/// One attempt to post one event to one sink.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookAttempt {
    /// The sink, by its configured name.
    pub webhook: String,
    /// The delivery, the same across its attempts: what the receiver saw
    /// as `X-AgentDocker-Delivery`.
    pub delivery: String,
    /// The event's sequence number and wire kind.
    pub seq: u64,
    pub event: String,
    /// Which attempt this was, from 1.
    pub attempt: u32,
    /// When the attempt started.
    pub at: DateTime<Utc>,
    /// How long it took, connecting and sending together.
    pub duration_ms: u64,
    pub result: AttemptResult,
    /// The receiver's status code, when it answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Why there was no answer, in a word: `timeout`, `dns`, `connect`,
    /// `tls`, `io`, `protocol` or `other`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// When the next attempt is due, while one is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_retry: Option<DateTime<Utc>>,
}

/// The limit a `webhook_deliveries` request is answered with: the one it
/// named, at least one and at most [`LOG_PER_WEBHOOK`], or [`LOG_LIMIT`].
pub fn log_limit(asked: Option<usize>) -> usize {
    asked.unwrap_or(LOG_LIMIT).clamp(1, LOG_PER_WEBHOOK)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line says its result by wire name and leaves out what it does
    /// not have; the bounds fit inside each other.
    #[test]
    fn an_attempt_round_trips_and_omits_what_it_lacks() {
        let attempt = WebhookAttempt {
            webhook: "team".into(),
            delivery: "0123456789abcdef0123456789abcdef".into(),
            seq: 41,
            event: "question_asked".into(),
            attempt: 1,
            at: DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            duration_ms: 12,
            result: AttemptResult::GaveUp,
            status: Some(404),
            error: None,
            next_retry: None,
        };
        let json = serde_json::to_value(&attempt).unwrap();
        assert_eq!(json["result"], "gave_up");
        assert_eq!(json["result"], AttemptResult::GaveUp.as_str());
        assert!(json.get("error").is_none() && json.get("next_retry").is_none());
        assert_eq!(
            serde_json::from_value::<WebhookAttempt>(json).unwrap(),
            attempt
        );
        assert_eq!(log_limit(None), LOG_LIMIT);
        assert_eq!(log_limit(Some(0)), 1);
        assert_eq!(log_limit(Some(10_000)), LOG_PER_WEBHOOK);
        const { assert!(LOG_LIMIT <= LOG_PER_WEBHOOK) };
        const {
            assert!(
                LOG_PER_WEBHOOK * crate::config::WEBHOOKS < LOG_TOTAL,
                "every configured sink keeps its whole log"
            )
        };
    }
}
