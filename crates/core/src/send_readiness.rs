//! A bounded readiness snapshot for the recipients actually queued by a send.
//! This is neither a receipt for the message nor a promise that a model will wake.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::readiness::{Decision, Fix, State};
use crate::{AgentId, AgentRecord, HUMAN_RUNTIME, ProviderIssueKind};

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
    /// The one thing that would get it taking messages, as
    /// [`crate::readiness::decide`] named it. Absent from an older daemon
    /// and for a session it does not know.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<Fix>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum SendIssue {
    UnknownSession,
    SessionEnded,
    /// Nothing of the session has reached AgentDocker: it cannot take this.
    Unreachable,
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
            Unreachable,
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
            Wire::Unreachable => Self::Unreachable,
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
            Self::Unreachable => "Can't receive messages",
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
            fix: None,
        }
    }

    /// The readiness a send reports for `agent`, from the one decision
    /// ([`crate::readiness::decide`]); none for a ready receiver or the
    /// person.
    pub fn for_agent(
        agent: &AgentRecord,
        now: DateTime<Utc>,
        blocked: Option<ProviderIssueKind>,
    ) -> Option<Self> {
        if agent.spec.runtime == HUMAN_RUNTIME {
            return None;
        }
        Self::from_decision(agent, &crate::readiness::decide(agent, now, blocked))
    }

    /// The wire form of a decision about `agent`: its state as the send
    /// issue it has always been, and its fix beside it.
    pub fn from_decision(agent: &AgentRecord, decision: &Decision) -> Option<Self> {
        let issue = match decision.state {
            State::Ended => SendIssue::SessionEnded,
            State::ProviderBlocked(kind) => SendIssue::ProviderBlocked(kind),
            State::Paused => SendIssue::DeliveryPaused,
            State::Silent => SendIssue::ReceiverSilent,
            State::Unreachable => SendIssue::Unreachable,
            State::AwaitsCheckIn | State::WaitsForPrompt => SendIssue::NoReceiver,
            State::Ready { .. } => return None,
        };
        Some(Self {
            agent: agent.id.clone(),
            name: plain(&agent.spec.name, 80),
            runtime: plain(&agent.spec.runtime, 40),
            issue,
            resume_session: session_word(agent).map(str::to_owned),
            fix: Some(decision.fix.clone()),
        })
    }

    /// The decided state this readiness reports. A session that waits for
    /// its check-in and one that waits for its next prompt share the wire
    /// kind `no_receiver`; the fix tells them apart.
    pub fn state(&self) -> Option<State> {
        Some(match self.issue {
            SendIssue::SessionEnded => State::Ended,
            SendIssue::ProviderBlocked(kind) => State::ProviderBlocked(kind),
            SendIssue::DeliveryPaused => State::Paused,
            SendIssue::ReceiverSilent => State::Silent,
            SendIssue::Unreachable => State::Unreachable,
            SendIssue::NoReceiver if self.fix == Some(Fix::WaitForCheckIn) => State::AwaitsCheckIn,
            SendIssue::NoReceiver => State::WaitsForPrompt,
            SendIssue::UnknownSession | SendIssue::Unknown => return None,
        })
    }

    /// The decision this readiness carries, when it names both.
    pub fn decision(&self) -> Option<Decision> {
        Some(Decision {
            state: self.state()?,
            fix: self.fix.clone()?,
        })
    }

    /// The words for where it stands: the decided state's, else the issue's.
    pub fn label(&self) -> &'static str {
        self.state().map_or(self.issue.label(), State::label)
    }

    /// The fix in a few words, beside the command or the button that does
    /// it. The long form, for an agent reading a send result, is
    /// [`Self::guidance`].
    pub fn hint(&self) -> &'static str {
        match self.decision() {
            Some(decision) => decision.hint(),
            None if self.state().is_some() => "See agentdocker ps --input-details for the fix.",
            None => "Check it before sending again.",
        }
    }

    /// Why it cannot take messages now and the one fix, in full.
    pub fn guidance(&self) -> String {
        if let Some(decision) = self.decision() {
            return decision.guidance(&self.runtime, &self.agent);
        }
        match (self.state(), self.issue) {
            (Some(state), _) => format!(
                "{} This daemon did not say what would fix it; agentdocker ps --input-details does.",
                crate::readiness::why(state, &self.runtime)
            ),
            (None, SendIssue::UnknownSession) => "Check the destination in AgentDocker before sending again; whether the message arrived is not known.".into(),
            (None, _) => "The message was queued; check delivery details before sending again. This app does not recognize the newer delivery status.".into(),
        }
    }
}

/// An agent's provider session id, when it is one a copied command can
/// carry as one plain word: never shell syntax, never a flag.
pub(crate) fn session_word(agent: &AgentRecord) -> Option<&str> {
    agent
        .spec
        .labels
        .get("session_id")
        .map(String::as_str)
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 128
                && id.as_bytes()[0].is_ascii_alphanumeric()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}

/// The one command that brings a Claude Code session back with the
/// AgentDocker channel, exactly as it is pasted into a terminal: into its
/// own folder, its own conversation, with the channel. None for another
/// runtime, without a conversation id, or for a folder that cannot be
/// written as one plain quoted word. `windows` writes it for PowerShell.
pub fn claude_resume_command(agent: &AgentRecord, windows: bool) -> Option<String> {
    if agent.spec.runtime != "claude-code" {
        return None;
    }
    let session = session_word(agent)?;
    let folder = agent
        .spec
        .workdir
        .as_deref()
        .or_else(|| agent.project.as_ref().map(|p| p.root.as_path()))?
        .to_str()?;
    if folder.is_empty() || folder.chars().any(char::is_control) {
        return None;
    }
    let resume = format!(
        "claude --resume {session} --dangerously-load-development-channels server:agentdocker"
    );
    Some(if windows {
        // PowerShell reads the curly single quotes as quotes too; each is
        // doubled inside the literal. Claude starts only once the folder is
        // entered: a folder that is gone runs nothing.
        let folder: String = folder
            .chars()
            .flat_map(|c| {
                let quote = matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}');
                std::iter::once(c).chain(quote.then_some(c))
            })
            .collect();
        format!(
            "if (Set-Location -LiteralPath '{folder}' -PassThru -ErrorAction SilentlyContinue) {{ $env:AGENTDOCKER_CLAUDE_CHANNEL_INPUT='1'; {resume} }}"
        )
    } else {
        format!(
            "cd '{}' && AGENTDOCKER_CLAUDE_CHANNEL_INPUT=1 {resume}",
            folder.replace('\'', "'\\''")
        )
    })
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
    /// The copied command is the whole thing to paste, for that session
    /// and no other: its folder, its conversation, the channel; quoted so a
    /// folder's name is never shell syntax.
    #[test]
    fn a_claude_session_gets_one_command_to_paste() {
        let mut agent = AgentRecord::new(
            crate::AgentSpec {
                name: "claude-code-1".into(),
                runtime: "claude-code".into(),
                workdir: Some("/work/it's here".into()),
                ..Default::default()
            },
            false,
            Utc::now(),
        );
        assert_eq!(
            claude_resume_command(&agent, false),
            None,
            "no conversation id"
        );
        agent
            .spec
            .labels
            .insert("session_id".into(), "1a2b3c4d-5e6f-4a7b".into());
        assert_eq!(
            claude_resume_command(&agent, false).unwrap(),
            "cd '/work/it'\\''s here' && AGENTDOCKER_CLAUDE_CHANNEL_INPUT=1 claude --resume 1a2b3c4d-5e6f-4a7b --dangerously-load-development-channels server:agentdocker"
        );
        assert_eq!(
            claude_resume_command(&agent, true).unwrap(),
            "if (Set-Location -LiteralPath '/work/it''s here' -PassThru -ErrorAction SilentlyContinue) { $env:AGENTDOCKER_CLAUDE_CHANNEL_INPUT='1'; claude --resume 1a2b3c4d-5e6f-4a7b --dangerously-load-development-channels server:agentdocker }"
        );
        // Curly quotes are quotes to PowerShell: doubled, so a folder's
        // name cannot end the literal and start a statement.
        agent.spec.workdir =
            Some("C:\\work\\Jane\u{2019}s \u{2018}x\u{2019}; Write-Output injected; #".into());
        let command = claude_resume_command(&agent, true).unwrap();
        assert!(
            command.contains(
                "Jane\u{2019}\u{2019}s \u{2018}\u{2018}x\u{2019}\u{2019}; Write-Output injected; #'"
            ),
            "{command}"
        );
        agent.spec.workdir = Some("/work/it's here".into());
        agent
            .spec
            .labels
            .insert("session_id".into(), "--resume; rm".into());
        assert_eq!(
            claude_resume_command(&agent, false),
            None,
            "never a flag or syntax"
        );
        agent.spec.labels.insert("session_id".into(), "abc".into());
        agent.spec.workdir = Some("/work/two\nlines".into());
        assert_eq!(claude_resume_command(&agent, false), None);
        agent.spec.runtime = "codex".into();
        assert_eq!(claude_resume_command(&agent, false), None, "Claude's only");
    }

    /// Every issue says its fix in one short line, whatever fix the wire
    /// carries, a newer one or none included.
    #[test]
    fn every_hint_is_one_short_line() {
        for runtime in ["claude-code", "codex", "opencode", "custom"] {
            for issue in [
                SendIssue::Unknown,
                SendIssue::UnknownSession,
                SendIssue::SessionEnded,
                SendIssue::NoReceiver,
                SendIssue::ReceiverSilent,
                SendIssue::Unreachable,
                SendIssue::DeliveryPaused,
                SendIssue::ProviderBlocked(ProviderIssueKind::Rate),
            ] {
                for fix in [
                    None,
                    Some(Fix::Reconnect {
                        command: "cd '/w' && claude".into(),
                    }),
                    Some(Fix::SetupThenReconnect),
                    Some(Fix::CodexResume {
                        session: "abc".into(),
                    }),
                    Some(Fix::LaunchLive),
                    Some(Fix::WaitForProvider),
                    Some(Fix::ReviewDelivery),
                    Some(Fix::ReadInbox),
                    Some(Fix::WaitForCheckIn),
                    Some(Fix::None),
                    Some(Fix::Other),
                ] {
                    let readiness = RecipientReadiness {
                        agent: AgentId::from("a".to_owned()),
                        name: "a".into(),
                        runtime: runtime.into(),
                        issue,
                        resume_session: None,
                        fix,
                    };
                    let hint = readiness.hint();
                    assert!(hint.len() <= 60 && !hint.contains('\n'), "{hint}");
                    assert!(!readiness.guidance().is_empty());
                }
            }
        }
    }

    use super::*;

    #[test]
    fn a_session_nothing_reaches_is_said_to_be_unreachable_with_what_connects_it() {
        let birth = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let mut agent = AgentRecord::new(
            crate::AgentSpec {
                name: "codex-sample".into(),
                runtime: "codex".into(),
                ..Default::default()
            },
            false,
            birth,
        );
        agent.status = crate::AgentStatus::Running;
        agent.started_at = Some(birth);
        agent.process_started_at = Some(birth);
        agent
            .spec
            .labels
            .insert("session_id".into(), "de4e3b54-48e1".into());
        let later = birth + crate::input::FIRST_CONTACT_GRACE;
        let readiness = RecipientReadiness::for_agent(&agent, later, None).unwrap();
        assert_eq!(readiness.issue, SendIssue::Unreachable);
        assert_eq!(readiness.issue.label(), "Can't receive messages");
        assert!(readiness.guidance().contains("codex resume de4e3b54-48e1"));
        let wire = serde_json::to_value(readiness.issue).unwrap();
        assert_eq!(wire, serde_json::json!({"kind": "unreachable"}));
        assert_eq!(
            serde_json::from_value::<SendIssue>(wire).unwrap(),
            SendIssue::Unreachable
        );
    }
    use crate::{AgentSpec, AgentStatus, InputDelivery};

    #[test]
    fn newer_readiness_kinds_do_not_turn_a_committed_send_into_a_decode_failure() {
        for (wire_issue, wire_fix, expected, fix) in [
            (
                serde_json::json!({"kind": "future_status", "detail": {"new": true}}),
                serde_json::Value::Null,
                SendIssue::Unknown,
                None,
            ),
            (
                serde_json::json!({"kind": "provider_blocked", "detail": "future_limit"}),
                serde_json::json!({"kind": "wait_for_provider"}),
                SendIssue::ProviderBlocked(ProviderIssueKind::Unknown),
                Some(Fix::WaitForProvider),
            ),
            (
                serde_json::json!({"kind": "no_receiver"}),
                serde_json::json!({"kind": "future_fix", "detail": {"new": true}}),
                SendIssue::NoReceiver,
                Some(Fix::Other),
            ),
        ] {
            let response: crate::Response = serde_json::from_value(serde_json::json!({
                "type": "sent", "message": "accepted-id", "subscribers": 0,
                "recipient_readiness": {"recipients": 1, "needs_attention": 1, "details": [{
                    "agent": "recipient", "name": "Reviewer", "runtime": "custom", "issue": wire_issue,
                    "fix": wire_fix
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
            assert_eq!(readiness.details[0].fix, fix);
            assert!(!readiness.details[0].guidance().is_empty());
        }
    }

    #[test]
    fn an_ended_claude_session_with_no_folder_is_never_told_to_run_a_command() {
        let mut ended = agent("claude-code");
        ended.status = AgentStatus::Exited { code: Some(0) };
        ended.spec.labels.insert(
            "session_id".into(),
            "0b5c5e2e-0000-4000-8000-000000000001".into(),
        );
        let readiness = RecipientReadiness::for_agent(&ended, ended.created_at, None).unwrap();
        assert_eq!(readiness.issue, SendIssue::SessionEnded);
        assert_eq!(readiness.fix, Some(Fix::None), "no folder, no command");
        assert_eq!(readiness.hint(), "Its messages wait until it is resumed.");
        ended.spec.workdir = Some("/p/orchard".into());
        let readiness = RecipientReadiness::for_agent(&ended, ended.created_at, None).unwrap();
        let command = claude_resume_command(&ended, cfg!(windows)).unwrap();
        assert_eq!(readiness.fix, Some(Fix::Reconnect { command }));
        assert_eq!(readiness.hint(), "Run this in a terminal to bring it back.");
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
        // OpenCode is woken by its plugin; one that never reached
        // AgentDocker is told how to get it.
        let mut opencode = agent("opencode");
        let guidance = RecipientReadiness::for_agent(&opencode, opencode.created_at, None)
            .unwrap()
            .guidance();
        assert!(guidance.contains("woken for it when idle"), "{guidance}");
        assert!(!guidance.contains("setup"), "one fix, not two: {guidance}");
        opencode.started_at = Some(opencode.created_at);
        let later = opencode.created_at + crate::input::FIRST_CONTACT_GRACE;
        let guidance = RecipientReadiness::for_agent(&opencode, later, None)
            .unwrap()
            .guidance();
        assert!(
            guidance.contains("agentdocker setup opencode"),
            "{guidance}"
        );
        let codex = agent("codex");
        assert!(
            RecipientReadiness::for_agent(&codex, codex.created_at, None)
                .unwrap()
                .guidance()
                .contains("takes them live")
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
        assert_eq!(
            issue.fix,
            Some(Fix::SetupThenReconnect),
            "no folder to resume in"
        );
        agent.spec.workdir = Some("/work/orchard".into());
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
