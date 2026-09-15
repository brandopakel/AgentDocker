//! Provider availability is independent of process liveness and input receipts.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::AgentRecord;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderIssueKind {
    Usage,
    Rate,
    Budget,
    Billing,
    Concurrency,
    Context,
    Authentication,
    Transport,
    Unknown,
}

impl ProviderIssueKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Usage => "Usage limit",
            Self::Rate => "Rate limit",
            Self::Budget => "Session budget reached",
            Self::Billing => "Provider credit unavailable",
            Self::Concurrency => "Provider capacity reached",
            Self::Context => "Conversation context full",
            Self::Authentication => "Provider sign-in needed",
            Self::Transport => "Provider unavailable",
            Self::Unknown => "Provider availability unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderIssue {
    pub kind: ProviderIssueKind,
    /// Provider-supplied only. Passing this time never clears a block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<DateTime<Utc>>,
    /// Explicit non-secret membership, matching the `provider-quota` label.
    /// Runtime/provider names alone never establish a shared account quota.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl ProviderIssue {
    pub fn local(kind: ProviderIssueKind) -> Self {
        Self {
            kind,
            reset_at: None,
            quota_group: None,
            model: None,
        }
    }

    pub fn valid_for(&self, source: &AgentRecord) -> bool {
        fn plain(value: &str) -> bool {
            !value.trim().is_empty()
                && value.len() <= 128
                && !value.chars().any(|c| {
                    c.is_control()
                        || matches!(c as u32,
                    0x061c | 0x200e..=0x200f | 0x202a..=0x202e | 0x2066..=0x2069)
                })
        }
        self.quota_group.as_ref().is_none_or(|group| {
            plain(group) && source.spec.labels.get("provider-quota") == Some(group)
        }) && self
            .model
            .as_ref()
            .is_none_or(|model| plain(model) && source.spec.model.as_ref() == Some(model))
    }

    pub fn affects(&self, source: &AgentRecord, target: &AgentRecord) -> bool {
        source.id == target.id
            || self.quota_group.as_ref().is_some_and(|group| {
                source.spec.labels.get("provider-quota") == Some(group)
                    && target.spec.labels.get("provider-quota") == Some(group)
                    && source.spec.provider == target.spec.provider
                    && self
                        .model
                        .as_ref()
                        .is_none_or(|model| target.spec.model.as_ref() == Some(model))
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAvailability {
    pub process_started_at: DateTime<Utc>,
    pub observed_at: DateTime<Utc>,
    /// None means this block was reconciled, not proof that a model is ready.
    pub issue: Option<ProviderIssue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleared_observation: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderReport {
    Blocked {
        issue: ProviderIssue,
    },
    /// A supported provider success signal, after reconciling the interrupted
    /// turn. The named observation prevents a late recovery clearing a new limit.
    Recovered {
        blocked_at: DateTime<Utc>,
    },
}

/// Shared quota blocks remain effective across process/daemon restarts and
/// expired reset times. Neither PID changes nor heartbeats clear them.
pub fn provider_block<'a>(
    target: &AgentRecord,
    agents: impl IntoIterator<Item = &'a AgentRecord>,
) -> Option<(&'a AgentRecord, &'a ProviderAvailability)> {
    agents
        .into_iter()
        .filter_map(|source| {
            let availability = source.provider_availability.as_ref()?;
            availability
                .issue
                .as_ref()?
                .affects(source, target)
                .then_some((source, availability))
        })
        .min_by_key(|(source, state)| (state.observed_at, source.id.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotas_require_explicit_membership_and_preserve_model_boundaries() {
        let now = DateTime::from_timestamp(1000, 0).unwrap();
        let mut a = AgentRecord::new(crate::AgentSpec::default(), false, now);
        a.spec.provider = Some("provider".into());
        a.spec.model = Some("model-a".into());
        a.spec
            .labels
            .insert("provider-quota".into(), "shared-account".into());
        let mut b = a.clone();
        b.id = crate::AgentId::generate();
        let mut issue = ProviderIssue::local(ProviderIssueKind::Usage);
        assert!(
            !issue.affects(&a, &b),
            "same provider/model is not proof of a shared quota"
        );
        issue.quota_group = Some("shared-account".into());
        issue.model = Some("model-a".into());
        assert!(issue.valid_for(&a) && issue.affects(&a, &b));
        b.spec.model = Some("model-b".into());
        assert!(!issue.affects(&a, &b));
        issue.model = None;
        assert!(issue.affects(&a, &b));
        b.spec.labels.clear();
        assert!(!issue.affects(&a, &b));
        issue.quota_group = Some("unrelated-account".into());
        assert!(!issue.valid_for(&a));
    }

    #[test]
    fn old_limits_survive_restart_and_are_not_cleared_by_time() {
        let now = DateTime::from_timestamp(1000, 0).unwrap();
        let mut a = AgentRecord::new(crate::AgentSpec::default(), false, now);
        let mut issue = ProviderIssue::local(ProviderIssueKind::Rate);
        issue.reset_at = Some(now);
        a.provider_availability = Some(ProviderAvailability {
            process_started_at: now,
            observed_at: now,
            issue: Some(issue),
            cleared_observation: None,
        });
        a.process_started_at = Some(now + chrono::Duration::days(1));
        assert!(provider_block(&a, [&a]).is_some());
        a.provider_availability.as_mut().unwrap().issue = None;
        assert!(provider_block(&a, [&a]).is_none());
    }
}
