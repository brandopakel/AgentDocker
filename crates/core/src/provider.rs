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
    #[serde(other)]
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
            plain(group)
                && source.spec.labels.get("provider-quota") == Some(group)
                && source.spec.provider.as_deref().is_some_and(plain)
        }) && self
            .model
            .as_ref()
            .is_none_or(|model| plain(model) && source.spec.model.as_ref() == Some(model))
    }

    pub fn affects(&self, source: &AgentRecord, target: &AgentRecord) -> bool {
        source.id == target.id
            || self.quota_group.as_ref().is_some_and(|group| {
                self.valid_for(source)
                    && source.spec.labels.get("provider-quota") == Some(group)
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
    /// How many times in a row the daemon has lifted a transient block to
    /// try the provider again (`retry_at`); none once a turn goes through.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub retries: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// A transient provider failure is retried, not waited on for ever: this
/// long after an agent's own transport failure the daemon lifts the block
/// so its next turn tries the provider again, twice as long after each
/// retry that failed in turn, and never longer than [`RETRY_LONGEST`].
pub const RETRY_FIRST: chrono::Duration = chrono::Duration::minutes(1);
pub const RETRY_LONGEST: chrono::Duration = chrono::Duration::minutes(30);
/// A transport failure this soon after a retry is that retry failing.
pub const RETRY_FAILED_WITHIN: chrono::Duration = chrono::Duration::minutes(10);

impl ProviderAvailability {
    /// When the daemon lifts this block to try the provider again: only an
    /// agent's own transport failure, with no reset time from the provider
    /// and no account it shares. A limit, a sign-in or an unknown failure
    /// waits for the provider or the person, as before.
    pub fn retry_at(&self) -> Option<DateTime<Utc>> {
        let issue = self.issue.as_ref()?;
        (issue.kind == ProviderIssueKind::Transport
            && issue.reset_at.is_none()
            && issue.quota_group.is_none())
        .then(|| {
            let wait = RETRY_FIRST * 2i32.saturating_pow(self.retries.min(16));
            self.observed_at + wait.min(RETRY_LONGEST)
        })
    }
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
    fn only_an_agents_own_transport_failure_is_retried_and_each_retry_waits_longer() {
        let at = DateTime::from_timestamp(1000, 0).unwrap();
        let block = |kind, retries| ProviderAvailability {
            process_started_at: at,
            observed_at: at,
            issue: Some(ProviderIssue::local(kind)),
            cleared_observation: None,
            retries,
        };
        assert_eq!(
            block(ProviderIssueKind::Transport, 0).retry_at(),
            Some(at + RETRY_FIRST)
        );
        assert_eq!(
            block(ProviderIssueKind::Transport, 2).retry_at(),
            Some(at + RETRY_FIRST * 4)
        );
        assert_eq!(
            block(ProviderIssueKind::Transport, 40).retry_at(),
            Some(at + RETRY_LONGEST),
            "never longer than the longest wait"
        );
        for kind in [
            ProviderIssueKind::Usage,
            ProviderIssueKind::Rate,
            ProviderIssueKind::Authentication,
            ProviderIssueKind::Context,
            ProviderIssueKind::Unknown,
        ] {
            assert_eq!(block(kind, 0).retry_at(), None, "{kind:?} waits");
        }
        let mut provider_said = block(ProviderIssueKind::Transport, 0);
        provider_said.issue.as_mut().unwrap().reset_at = Some(at);
        assert_eq!(provider_said.retry_at(), None, "the provider's time stands");
        let mut shared = block(ProviderIssueKind::Transport, 0);
        shared.issue.as_mut().unwrap().quota_group = Some("account".into());
        assert_eq!(
            shared.retry_at(),
            None,
            "an account's block is not one agent's"
        );
        let mut lifted = block(ProviderIssueKind::Transport, 1);
        lifted.issue = None;
        assert_eq!(lifted.retry_at(), None, "nothing to lift");
        // Stored without the count until there is one, and read back so.
        let stored = serde_json::to_value(block(ProviderIssueKind::Transport, 0)).unwrap();
        assert!(stored.get("retries").is_none());
        assert_eq!(
            serde_json::from_value::<ProviderAvailability>(stored)
                .unwrap()
                .retries,
            0
        );
    }

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
            retries: 0,
        });
        a.process_started_at = Some(now + chrono::Duration::days(1));
        assert!(provider_block(&a, [&a]).is_some());
        a.provider_availability.as_mut().unwrap().issue = None;
        assert!(provider_block(&a, [&a]).is_none());
    }

    #[test]
    fn shared_quotas_require_a_known_provider_even_for_stored_reports() {
        let now = DateTime::from_timestamp(1000, 0).unwrap();
        let mut source = AgentRecord::new(crate::AgentSpec::default(), false, now);
        source
            .spec
            .labels
            .insert("provider-quota".into(), "account".into());
        let mut peer = source.clone();
        peer.id = crate::AgentId::generate();
        let mut issue = ProviderIssue::local(ProviderIssueKind::Usage);
        issue.quota_group = Some("account".into());
        for provider in [
            None,
            Some(""),
            Some(" "),
            Some("provider\n"),
            Some("\u{202e}provider"),
        ] {
            source.spec.provider = provider.map(String::from);
            peer.spec.provider = source.spec.provider.clone();
            assert!(!issue.valid_for(&source));
            source.provider_availability = Some(ProviderAvailability {
                process_started_at: now,
                observed_at: now,
                issue: Some(issue.clone()),
                cleared_observation: None,
                retries: 0,
            });
            assert!(provider_block(&source, [&source, &peer]).is_some());
            assert!(provider_block(&peer, [&source, &peer]).is_none());
        }
        source.spec.provider = Some("provider-a".into());
        peer.spec.provider = Some("provider-b".into());
        assert!(issue.valid_for(&source));
        assert!(!issue.affects(&source, &peer));
        peer.spec.provider = source.spec.provider.clone();
        assert!(issue.affects(&source, &peer));
        issue.quota_group = None;
        source.spec.provider = None;
        assert!(
            issue.valid_for(&source),
            "a local report needs no inferred provider"
        );
        assert!(!issue.affects(&source, &peer));
    }
}
