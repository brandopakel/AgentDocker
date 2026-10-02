//! Keep a turn's structured failure when its final error loses the detail.
use agentdocker_core::{ProviderIssue, ProviderIssueKind};
use serde_json::Value;

#[derive(Default)]
pub(super) struct TurnFailures(Option<(String, ProviderIssue)>);

impl TurnFailures {
    pub fn observe(
        &mut self,
        active: &str,
        reported: Option<&str>,
        error: &Value,
    ) -> Option<ProviderIssue> {
        if reported != Some(active) {
            return None;
        }
        let mut issue = crate::provider_status::codex(error);
        if issue.kind == ProviderIssueKind::Unknown
            && let Some((turn, previous)) = &self.0
            && turn == active
        {
            issue = previous.clone();
        }
        // Only normalized fields survive; error messages can contain secrets.
        self.0 = Some((active.to_owned(), issue.clone()));
        Some(issue)
    }

    pub fn clear(&mut self) {
        self.0 = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retry_exhaustion_keeps_the_same_turns_authentication_failure() {
        let mut failures = TurnFailures::default();
        let auth = json!({"codexErrorInfo":{"httpConnectionFailed":{"httpStatusCode":401}},
            "message":"secret-bearing provider text must not be retained"});
        let other = json!({"codexErrorInfo":"other"});
        for error in [&auth, &auth, &other, &other] {
            assert_eq!(
                failures.observe("turn", Some("turn"), error),
                Some(ProviderIssue::local(ProviderIssueKind::Authentication))
            );
        }
    }

    #[test]
    fn a_new_structured_failure_replaces_the_previous_reason() {
        let mut failures = TurnFailures::default();
        for (code, kind) in [
            ("unauthorized", ProviderIssueKind::Authentication),
            ("usageLimitExceeded", ProviderIssueKind::Usage),
            ("serverOverloaded", ProviderIssueKind::Transport),
            ("other", ProviderIssueKind::Transport),
        ] {
            assert_eq!(
                failures.observe("turn", Some("turn"), &json!({"codexErrorInfo":code})),
                Some(ProviderIssue::local(kind))
            );
        }
    }

    #[test]
    fn stale_and_unbound_errors_cannot_poison_the_active_turn() {
        let mut failures = TurnFailures::default();
        let auth = json!({"codexErrorInfo":"unauthorized"});
        assert_eq!(failures.observe("turn", Some("stale"), &auth), None);
        assert_eq!(failures.observe("turn", None, &auth), None);
        assert_eq!(
            failures.observe("turn", Some("turn"), &json!({})),
            Some(ProviderIssue::local(ProviderIssueKind::Unknown))
        );
        failures.observe("turn", Some("turn"), &auth);
        assert_eq!(
            failures.observe("next", Some("next"), &json!({})),
            Some(ProviderIssue::local(ProviderIssueKind::Unknown))
        );
    }

    #[test]
    fn finishing_a_turn_discards_its_failure_details() {
        let mut failures = TurnFailures::default();
        failures.observe(
            "turn",
            Some("turn"),
            &json!({"codexErrorInfo":"unauthorized"}),
        );
        failures.clear();
        assert_eq!(
            failures.observe(
                "turn",
                Some("turn"),
                &json!({"message":"401 sign-in needed"})
            ),
            Some(ProviderIssue::local(ProviderIssueKind::Unknown)),
            "neither old-turn state nor terminal prose establishes a new failure"
        );
    }
}
