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
                InputReadiness::Unreachable => SendIssue::Unreachable,
                InputReadiness::Unverified => SendIssue::NoReceiver,
                InputReadiness::Paused => SendIssue::DeliveryPaused,
                InputReadiness::Stale => SendIssue::ReceiverSilent,
                InputReadiness::AwaitingFirstReceipt | InputReadiness::Verified => return None,
            }
        };
        let resume_session = session_word(agent).map(str::to_owned);
        Some(Self {
            agent: agent.id.clone(),
            name: plain(&agent.spec.name, 80),
            runtime: plain(&agent.spec.runtime, 40),
            issue,
            resume_session,
        })
    }

    /// The fix in a few words, beside the command or the button that does
    /// it. The long form, for an agent reading a send result, is
    /// [`Self::guidance`].
    pub fn hint(&self) -> &'static str {
        let claude = self.runtime == "claude-code";
        match self.issue {
            SendIssue::Unknown | SendIssue::UnknownSession => "Check it before sending again.",
            SendIssue::ProviderBlocked(_) => "Resume delivery once the provider is back.",
            SendIssue::DeliveryPaused => "Review its delivery before reconnecting it.",
            SendIssue::SessionEnded if claude && self.resume_session.is_some() => {
                "Run this in a terminal to bring it back."
            }
            SendIssue::SessionEnded => "Its messages wait until it is resumed.",
            SendIssue::Unreachable if claude && self.resume_session.is_some() => {
                "It never reached AgentDocker; run this to restart it."
            }
            SendIssue::Unreachable => "It never reached AgentDocker; its messages are kept.",
            SendIssue::NoReceiver | SendIssue::ReceiverSilent if claude => {
                if self.resume_session.is_some() {
                    "Quit it, then run this in a terminal."
                } else {
                    "Restart it with the AgentDocker channel."
                }
            }
            SendIssue::NoReceiver | SendIssue::ReceiverSilent if self.runtime == "codex" => {
                "It takes messages at its next prompt."
            }
            SendIssue::NoReceiver | SendIssue::ReceiverSilent if self.runtime == "opencode" => {
                "Its plugin wakes it; without it, at its next turn."
            }
            SendIssue::NoReceiver | SendIssue::ReceiverSilent => {
                "It takes messages when it reads its inbox."
            }
        }
    }

    pub fn guidance(&self) -> String {
        match self.issue {
            SendIssue::Unknown => "The message was queued; check delivery details before sending again. This app does not recognize the newer delivery status.".into(),
            SendIssue::UnknownSession => "Check the destination in AgentDocker before sending again; whether the message arrived is not known.".into(),
            SendIssue::ProviderBlocked(_) => "Check the provider limit or sign-in, then explicitly resume input after recovery.".into(),
            SendIssue::DeliveryPaused => "Review this session's delivery in AgentDocker before reconnecting it; its messages are kept.".into(),
            SendIssue::SessionEnded => "Resume this conversation from its project folder; its messages are kept until it takes them.".into(),
            // Kept, but nothing of the session will read them: say what
            // connects it, never "may wait".
            SendIssue::Unreachable if self.runtime == "codex" => match &self.resume_session {
                Some(session) => format!("Nothing of this Codex session has reached AgentDocker, so it can't take messages; they are kept. Resume it from its project folder with: codex resume {session}, or start it from Launch agent."),
                None => "Nothing of this Codex session has reached AgentDocker, so it can't take messages; they are kept. A Codex started as plain `codex` names no conversation AgentDocker can find: resume it by id from its project folder (codex resume <id>), or start it from Launch agent.".into(),
            },
            SendIssue::Unreachable if self.runtime == "claude-code" => "Nothing of this Claude session has reached AgentDocker: its hooks and MCP server aren't running for it, so it can't take messages; they are kept. Run agentdocker setup claude-code, then restart it from its project folder.".into(),
            SendIssue::Unreachable => format!(
                "Nothing of this {} session has reached AgentDocker, so it can't take messages; they are kept. It reads them only through agentdocker inbox --as {}.",
                self.runtime, self.agent.short()
            ),
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
            SendIssue::NoReceiver | SendIssue::ReceiverSilent if self.runtime == "codex" => "This Codex session takes messages at its next prompt; nothing can wake it in its terminal. A Codex started from AgentDocker's Launch agent takes them live.".into(),
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

/// An agent's provider session id, when it is one a copied command can
/// carry as one plain word: never shell syntax, never a flag.
fn session_word(agent: &AgentRecord) -> Option<&str> {
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
            .insert("session_id".into(), "3ebbfa91-6e52-4cea".into());
        assert_eq!(
            claude_resume_command(&agent, false).unwrap(),
            "cd '/work/it'\\''s here' && AGENTDOCKER_CLAUDE_CHANNEL_INPUT=1 claude --resume 3ebbfa91-6e52-4cea --dangerously-load-development-channels server:agentdocker"
        );
        assert_eq!(
            claude_resume_command(&agent, true).unwrap(),
            "if (Set-Location -LiteralPath '/work/it''s here' -PassThru -ErrorAction SilentlyContinue) { $env:AGENTDOCKER_CLAUDE_CHANNEL_INPUT='1'; claude --resume 3ebbfa91-6e52-4cea --dangerously-load-development-channels server:agentdocker }"
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

    /// Every issue says its fix in one short line.
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
                for resume_session in [None, Some("abc".to_owned())] {
                    let hint = RecipientReadiness {
                        agent: AgentId::from("a".to_owned()),
                        name: "a".into(),
                        runtime: runtime.into(),
                        issue,
                        resume_session,
                    }
                    .hint();
                    assert!(hint.len() <= 60 && !hint.contains('\n'), "{hint}");
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
                name: "codex-release".into(),
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
