//! A bounded readiness snapshot for the recipients actually queued by a send.
//! This is neither a receipt for the message nor a promise that a model will wake.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AgentId, AgentRecord, HUMAN_RUNTIME, InputReadiness, ProviderIssueKind};

pub const MAX_DETAILS: usize = 32;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendReadiness {
    pub recipients: usize,
    pub needs_attention: usize,
    pub details: Vec<RecipientReadiness>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipientReadiness {
    pub agent: AgentId,
    pub name: String,
    pub runtime: String,
    pub issue: SendIssue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_session: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum SendIssue {
    UnknownSession,
    SessionEnded,
    NoReceiver,
    ReceiverSilent,
    DeliveryPaused,
    ProviderBlocked(ProviderIssueKind),
    Unknown,
}

impl<'de> Deserialize<'de> for SendIssue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // An internally tagged wire representation discards an unknown kind's
        // fields too. An adjacently tagged unit fallback rejects a newer
        // variant with a non-unit `detail`, even though the send committed.
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum Wire {
            UnknownSession,
            SessionEnded,
            NoReceiver,
            ReceiverSilent,
            DeliveryPaused,
            ProviderBlocked {
                detail: ProviderIssueKind,
            },
            #[serde(other)]
            Unknown,
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::UnknownSession => Self::UnknownSession,
            Wire::SessionEnded => Self::SessionEnded,
            Wire::NoReceiver => Self::NoReceiver,
            Wire::ReceiverSilent => Self::ReceiverSilent,
            Wire::DeliveryPaused => Self::DeliveryPaused,
            Wire::ProviderBlocked { detail } => Self::ProviderBlocked(detail),
            Wire::Unknown => Self::Unknown,
        })
    }
}

impl SendIssue {
    pub fn label(self) -> &'static str {
        match self {
            Self::UnknownSession => "Session not found",
            Self::SessionEnded => "Session ended",
            Self::NoReceiver => "Messages may wait for its next prompt",
            Self::ReceiverSilent => "Not heard from recently",
            Self::DeliveryPaused => "Not receiving messages",
            Self::ProviderBlocked(kind) => kind.label(),
            Self::Unknown => "Delivery status unavailable",
        }
    }
}

fn plain(value: &str, limit: usize) -> String {
    value
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(*c as u32,
        0x061c | 0x200e..=0x200f | 0x202a..=0x202e | 0x2066..=0x2069)
        })
        .take(limit)
        .collect()
}

impl RecipientReadiness {
    pub fn unknown(agent: AgentId) -> Self {
        Self {
            name: plain(agent.as_str(), 80),
            agent,
            runtime: String::new(),
            issue: SendIssue::UnknownSession,
            resume_session: None,
        }
    }

    pub fn for_agent(
        agent: &AgentRecord,
        now: DateTime<Utc>,
        blocked: Option<ProviderIssueKind>,
    ) -> Option<Self> {
        if agent.spec.runtime == HUMAN_RUNTIME {
            return None;
        }
        let issue = if !agent.status.is_live() {
            SendIssue::SessionEnded
        } else if let Some(kind) = blocked {
            SendIssue::ProviderBlocked(kind)
        } else {
            match InputReadiness::for_agent(agent, now) {
                InputReadiness::SessionEnded => SendIssue::SessionEnded,
                InputReadiness::Unverified => SendIssue::NoReceiver,
                InputReadiness::Paused => SendIssue::DeliveryPaused,
                InputReadiness::Stale => SendIssue::ReceiverSilent,
                InputReadiness::AwaitingFirstReceipt | InputReadiness::Verified => return None,
            }
        };
        // A copied command must never interpret a label as shell syntax or a flag.
        let resume_session = agent
            .spec
            .labels
            .get("session_id")
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 128
                    && id.as_bytes()[0].is_ascii_alphanumeric()
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
            .cloned();
        Some(Self {
            agent: agent.id.clone(),
            name: plain(&agent.spec.name, 80),
            runtime: plain(&agent.spec.runtime, 40),
            issue,
            resume_session,
        })
    }

    pub fn guidance(&self) -> String {
        match self.issue {
            SendIssue::Unknown => "The message was queued; check delivery details before sending again. This app does not recognize the newer delivery status.".into(),
            SendIssue::UnknownSession => "Check the destination in AgentDocker before sending again; whether the message arrived is not known.".into(),
            SendIssue::ProviderBlocked(_) => "Check the provider limit or sign-in, then explicitly resume input after recovery.".into(),
            SendIssue::DeliveryPaused => "Review this session's delivery in AgentDocker before reconnecting it; its messages are kept.".into(),
            SendIssue::SessionEnded => "Resume this conversation from its project folder; its messages are kept until it takes them.".into(),
            SendIssue::NoReceiver | SendIssue::ReceiverSilent if self.runtime == "claude-code" => {
                match &self.resume_session {
                    Some(session) => format!("After saving work and exiting the current Claude session, resume from the same project folder with: AGENTDOCKER_CLAUDE_CHANNEL_INPUT=1 claude --resume {session} --dangerously-load-development-channels server:agentdocker. Complete Claude's channel consent; AgentDocker's MCP entry must include --claude-channel."),
                    None => "Resume Claude from its project folder with AGENTDOCKER_CLAUDE_CHANNEL_INPUT=1 and --dangerously-load-development-channels server:agentdocker, then complete its channel consent. See Tools for AgentDocker setup.".into(),
                }
            }
            // OpenCode's plugin watches an idle session and wakes it; a
            // session started without the plugin only reads its queue itself.
            SendIssue::NoReceiver | SendIssue::ReceiverSilent if self.runtime == "opencode" => format!(
                "An OpenCode session with AgentDocker's plugin takes this at its next turn, and is woken for it when idle. Without the plugin (agentdocker setup opencode, then restart OpenCode) it waits until the agent reads its inbox (agentdocker inbox --as {}).",
                self.agent.short()
            ),
            SendIssue::NoReceiver | SendIssue::ReceiverSilent if self.runtime == "codex" => "Open this session's details in Tools and reconnect its message delivery. Until then, check its terminal: a waiting message cannot wake it on its own.".into(),
            // No adapter exists for this runtime: nothing here can wake
            // it, and saying "reconnect a receiver" would send the person
            // looking for one. The message waits in its queue for the
            // agent's own read.
            SendIssue::NoReceiver | SendIssue::ReceiverSilent => format!(
                "A {} session cannot be woken by a message: it waits until the agent reads its inbox (agentdocker inbox --as {} or watch --as {}, or the MCP read_inbox tool).",
                self.runtime, self.agent.short(), self.agent.short()
            ),
        }
    }
}

impl SendReadiness {
    pub fn observe(&mut self, recipient: Option<RecipientReadiness>) {
        self.recipients += 1;
        if let Some(recipient) = recipient {
            self.needs_attention += 1;
            if self.details.len() < MAX_DETAILS {
                self.details.push(recipient);
            }
        }
    }

    pub fn omitted(&self) -> usize {
        self.needs_attention.saturating_sub(self.details.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentSpec, AgentStatus, InputDelivery};

    #[test]
    fn newer_readiness_kinds_do_not_turn_a_committed_send_into_a_decode_failure() {
        for (wire_issue, expected) in [
            (
                serde_json::json!({"kind": "future_status", "detail": {"new": true}}),
                SendIssue::Unknown,
            ),
            (
                serde_json::json!({"kind": "provider_blocked", "detail": "future_limit"}),
                SendIssue::ProviderBlocked(ProviderIssueKind::Unknown),
            ),
        ] {
            let response: crate::Response = serde_json::from_value(serde_json::json!({
                "type": "sent", "message": "accepted-id", "subscribers": 0,
                "recipient_readiness": {"recipients": 1, "needs_attention": 1, "details": [{
                    "agent": "recipient", "name": "Reviewer", "runtime": "custom", "issue": wire_issue
                }]}
            })).unwrap();
            let crate::Response::Sent {
                message,
                recipient_readiness: Some(readiness),
                ..
            } = response
            else {
                panic!("successful send must remain successful");
            };
            assert_eq!(message.as_str(), "accepted-id");
            assert_eq!(readiness.details[0].issue, expected);
            assert!(!readiness.details[0].guidance().is_empty());
        }
    }

    fn agent(runtime: &str) -> AgentRecord {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let mut agent = AgentRecord::new(
            AgentSpec {
                name: "reviewer".into(),
                runtime: runtime.into(),
                ..Default::default()
            },
            false,
            now,
        );
        agent.status = AgentStatus::Running;
        agent.process_started_at = Some(now);
        agent
    }

    #[test]
    fn every_runtime_requires_current_receiver_evidence_and_limits_take_precedence() {
        for runtime in ["codex", "claude-code", "gemini-cli", "cursor", "custom"] {
            let mut agent = agent(runtime);
            let now = agent.created_at;
            let issue = |agent: &AgentRecord, blocked| {
                RecipientReadiness::for_agent(agent, now, blocked).map(|r| r.issue)
            };
            assert_eq!(issue(&agent, None), Some(SendIssue::NoReceiver));
            agent.input_delivery = Some(InputDelivery {
                process_started_at: now,
                reported_at: now,
                paused: false,
                pause_reason: None,
                received: None,
                received_at: None,
            });
            assert_eq!(issue(&agent, None), None);
            assert_eq!(
                issue(&agent, Some(ProviderIssueKind::Usage)),
                Some(SendIssue::ProviderBlocked(ProviderIssueKind::Usage))
            );
            agent.input_delivery.as_mut().unwrap().paused = true;
            assert_eq!(issue(&agent, None), Some(SendIssue::DeliveryPaused));
            agent.input_delivery.as_mut().unwrap().paused = false;
            agent.process_started_at = Some(now + chrono::Duration::seconds(1));
            assert_eq!(issue(&agent, None), Some(SendIssue::ReceiverSilent));
            agent.status = AgentStatus::Exited { code: Some(0) };
            assert_eq!(issue(&agent, None), Some(SendIssue::SessionEnded));
        }
        assert!(
            RecipientReadiness::for_agent(
                &agent(HUMAN_RUNTIME),
                agent(HUMAN_RUNTIME).created_at,
                None
            )
            .is_none()
        );
    }

    /// A runtime nothing here can wake is told so: the guidance names the
    /// agent's own read of its queue, by its id, and never sends the person
    /// to reconnect a receiver that cannot exist. Codex and Claude Code keep
    /// their receiver guidance.
    #[test]
    fn a_runtime_without_an_adapter_is_told_the_agent_reads_its_own_queue() {
        for runtime in ["custom", "gemini-cli", "cursor"] {
            let agent = agent(runtime);
            let issue = RecipientReadiness::for_agent(&agent, agent.created_at, None).unwrap();
            let guidance = issue.guidance();
            assert!(
                guidance.contains("cannot be woken by a message"),
                "{runtime}: {guidance}"
            );
            assert!(
                guidance.contains(&format!("inbox --as {}", agent.id.short())),
                "{runtime}: {guidance}"
            );
            assert!(!guidance.contains("reconnect"), "{runtime}: {guidance}");
        }
        // OpenCode is woken by its plugin, and told how to get it.
        let opencode = agent("opencode");
        let guidance = RecipientReadiness::for_agent(&opencode, opencode.created_at, None)
            .unwrap()
            .guidance();
        assert!(guidance.contains("woken for it when idle"), "{guidance}");
        assert!(
            guidance.contains("agentdocker setup opencode"),
            "{guidance}"
        );
        let codex = agent("codex");
        assert!(
            RecipientReadiness::for_agent(&codex, codex.created_at, None)
                .unwrap()
                .guidance()
                .contains("reconnect its message delivery")
        );
    }

    #[test]
    fn reconnect_guidance_never_interprets_session_labels_as_shell_code() {
        let mut agent = agent("claude-code");
        agent.spec.name = "unsafe\u{1b}[31m\n\u{202e}name".into();
        for session in [
            "--help",
            "$(touch secret)",
            "id; command",
            "id\nnext",
            "",
            "日本語",
        ] {
            agent
                .spec
                .labels
                .insert("session_id".into(), session.into());
            let issue = RecipientReadiness::for_agent(&agent, agent.created_at, None).unwrap();
            assert!(issue.resume_session.is_none());
            assert!(!issue.guidance().contains("claude --resume"));
            assert!(!issue.name.chars().any(char::is_control));
            assert!(!issue.name.contains('\u{202e}'));
        }
        agent
            .spec
            .labels
            .insert("session_id".into(), "session-123_ab".into());
        let issue = RecipientReadiness::for_agent(&agent, agent.created_at, None).unwrap();
        assert!(issue.guidance().contains("claude --resume session-123_ab --dangerously-load-development-channels server:agentdocker"));
    }

    #[test]
    fn large_fanout_counts_all_recipients_but_bounds_advice() {
        let mut summary = SendReadiness::default();
        for index in 0..1000 {
            summary.observe(Some(RecipientReadiness::unknown(
                format!("agent-{index}").into(),
            )));
        }
        summary.observe(None);
        assert_eq!(summary.recipients, 1001);
        assert_eq!(summary.needs_attention, 1000);
        assert_eq!(summary.details.len(), MAX_DETAILS);
        assert_eq!(summary.omitted(), 968);
        assert_eq!(summary.details[0].issue, SendIssue::UnknownSession);
        assert!(serde_json::to_vec(&summary).unwrap().len() < 10_000);
    }
}
