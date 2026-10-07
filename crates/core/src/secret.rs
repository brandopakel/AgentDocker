//! Temporary human answers use a separate wire route, never ordinary envelopes.
//! These wire types are serializable for IPC; no storage type contains them.
use crate::{AgentId, ProcessIdentity};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, de};
use std::{collections::BTreeMap, fmt};

pub const MAX_FIELDS: usize = 8;
pub const MAX_VALUE_BYTES: usize = 16_000;
pub const MAX_ANSWER_BYTES: usize = 64 * 1024;
pub const MAX_QUESTION_BYTES: usize = 32 * 1024;
pub const PROVIDER_NOTICE: &str = "Codex and the selected model receive these answers and may retain or repeat them. AgentDocker does not save this submission; provider output can still appear in ordinary history.";

fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

/// Explicitly exposed only at the destination. Debug/error output is redacted.
/// This does not promise locked memory, zeroization, or provider-side secrecy.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SecretText(String);

impl SecretText {
    pub fn new(value: String) -> Result<Self, &'static str> {
        if value.len() > MAX_VALUE_BYTES || value.contains('\0') {
            return Err("secret text exceeds its bound or contains NUL");
        }
        Ok(Self(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretText([redacted])")
    }
}

impl<'de> Deserialize<'de> for SecretText {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value =
            String::deserialize(d).map_err(|_| de::Error::custom("secret value must be text"))?;
        Self::new(value).map_err(de::Error::custom)
    }
}

/// A complete bundle, including ordinary fields when any field is secret.
/// Duplicate field IDs and oversized bundles are rejected during decoding.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SecretAnswers(BTreeMap<String, SecretText>);

impl SecretAnswers {
    pub fn new(values: BTreeMap<String, SecretText>) -> Result<Self, &'static str> {
        if values.is_empty()
            || values.len() > MAX_FIELDS
            || values.keys().any(|id| !identifier(id))
            || values.values().map(|v| v.expose().len()).sum::<usize>() > MAX_ANSWER_BYTES
        {
            return Err("secret answer bundle is invalid or exceeds its bound");
        }
        Ok(Self(values))
    }

    pub fn matches(&self, fields: &[SecretField]) -> bool {
        self.0.len() == fields.len() && fields.iter().all(|f| self.0.contains_key(&f.id))
    }

    pub fn allocated_bytes(&self) -> usize {
        self.0
            .iter()
            .map(|(key, value)| key.capacity().saturating_add(value.0.capacity()))
            .sum()
    }

    pub fn into_values(self) -> BTreeMap<String, SecretText> {
        self.0
    }
}

impl fmt::Debug for SecretAnswers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretAnswers([redacted])")
    }
}

impl<'de> Deserialize<'de> for SecretAnswers {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Answers;
        impl<'de> de::Visitor<'de> for Answers {
            type Value = SecretAnswers;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a bounded object of secret text answers")
            }
            fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut values = BTreeMap::new();
                let mut bytes = 0usize;
                while let Some((key, value)) = map
                    .next_entry::<String, SecretText>()
                    .map_err(|_| de::Error::custom("invalid secret answer bundle"))?
                {
                    bytes = bytes.saturating_add(value.expose().len());
                    if values.len() >= MAX_FIELDS
                        || bytes > MAX_ANSWER_BYTES
                        || !identifier(&key)
                        || values.insert(key, value).is_some()
                    {
                        return Err(de::Error::custom("invalid secret answer bundle"));
                    }
                }
                SecretAnswers::new(values).map_err(de::Error::custom)
            }
        }
        d.deserialize_map(Answers)
            .map_err(|_| de::Error::custom("invalid secret answer bundle"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretField {
    pub id: String,
    pub question: String,
    pub is_secret: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretReviewSpec {
    pub thread: String,
    pub turn: String,
    pub fields: Vec<SecretField>,
}

impl SecretReviewSpec {
    pub fn valid(&self) -> bool {
        identifier(&self.thread)
            && identifier(&self.turn)
            && !self.fields.is_empty()
            && self.fields.len() <= MAX_FIELDS
            && self.fields.iter().any(|f| f.is_secret)
            && self
                .fields
                .iter()
                .map(|f| f.id.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == self.fields.len()
            && self.fields.iter().all(|f| {
                identifier(&f.id)
                    && !f.question.trim().is_empty()
                    && f.question.len() <= MAX_VALUE_BYTES
            })
            && self.fields.iter().map(|f| f.question.len()).sum::<usize>() <= MAX_QUESTION_BYTES
    }
}

/// Public metadata only. Answers and the owner's capability are never listed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretReview {
    pub id: String,
    pub agent: AgentId,
    pub recipient: AgentId,
    pub owner: ProcessIdentity,
    pub request: SecretReviewSpec,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum SecretReply {
    Waiting,
    /// Taking this response consumes the stored answer before the socket write.
    Answered {
        answers: SecretAnswers,
    },
    Closed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Request;
    #[test]
    fn secret_wire_round_trip_is_explicit_but_debug_and_decode_errors_hide_values() {
        let canary = "synthetic-private-value";
        let answers = SecretAnswers::new(BTreeMap::from([(
            "field".into(),
            SecretText::new(canary.into()).unwrap(),
        )]))
        .unwrap();
        let request = Request::AnswerSecretReview {
            from: "human".into(),
            review: "route".into(),
            answers,
            retention_acknowledged: true,
        };
        let wire = serde_json::to_string(&request).unwrap();
        assert!(wire.contains(canary));
        let decoded: Request = serde_json::from_str(&wire).unwrap();
        assert_eq!(request, decoded);
        assert!(!format!("{request:?}").contains(canary));
        assert!(
            !serde_json::from_str::<SecretAnswers>(&format!("{{\"field\":{{\"{canary}\":1}}}}"))
                .unwrap_err()
                .to_string()
                .contains(canary)
        );
    }
    #[test]
    fn duplicate_fields_partial_bundles_and_memory_overflow_are_refused() {
        assert!(serde_json::from_str::<SecretAnswers>(r#"{"a":"one","a":"two"}"#).is_err());
        assert!(serde_json::from_str::<SecretAnswers>("{}").is_err());
        assert!(SecretText::new("x".repeat(MAX_VALUE_BYTES + 1)).is_err());
        let answers = SecretAnswers::new(BTreeMap::from([(
            "a".into(),
            SecretText::new("value".into()).unwrap(),
        )]))
        .unwrap();
        let fields = vec![
            SecretField {
                id: "a".into(),
                question: "A?".into(),
                is_secret: true,
            },
            SecretField {
                id: "b".into(),
                question: "B?".into(),
                is_secret: false,
            },
        ];
        assert!(!answers.matches(&fields));
        let oversized = (0..5)
            .map(|i| {
                (
                    i.to_string(),
                    SecretText::new("x".repeat(MAX_VALUE_BYTES)).unwrap(),
                )
            })
            .collect();
        assert!(SecretAnswers::new(oversized).is_err());
        let spec = SecretReviewSpec {
            thread: "thread".into(),
            turn: "turn".into(),
            fields,
        };
        assert!(spec.valid());
        let mut duplicate = spec.clone();
        duplicate.fields[1].id = "a".into();
        assert!(!duplicate.valid());
        let mut ordinary = spec;
        ordinary.fields[0].is_secret = false;
        assert!(!ordinary.valid());
    }
}
