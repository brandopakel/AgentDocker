//! Whether an agent can receive input now, and the one thing that would
//! change that. [`decide`] is the only place the provider block, the
//! receiver evidence and the kind of session are put together: the daemon's
//! send result, `ps`, the MCP tools and the desktop all render its
//! [`Decision`], so a label, a short hint and the long guidance never name
//! different fixes for the same session.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AgentId, AgentRecord, HUMAN_RUNTIME, InputReadiness, ProviderIssueKind};

/// Where an agent stands for input. The first that applies, in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// The session has ended; what was queued for it is kept.
    Ended,
    /// Its provider is limited or signed out. Takes precedence over any
    /// receiver evidence: a ready receiver cannot erase a limit.
    ProviderBlocked(ProviderIssueKind),
    /// Its receiver paused delivery for this process.
    Paused,
    /// Its receiver reported for this process and has gone quiet.
    Silent,
    /// Live, and nothing of this process has ever reached AgentDocker
    /// ([`crate::input::unreachable`]).
    Unreachable,
    /// It reads only when it checks in: a browser agent from the remote
    /// connector, or a session known by its key alone that nothing has been
    /// heard from for [`crate::agent::SESSION_PRESENCE`].
    AwaitsCheckIn,
    /// In touch, but nothing wakes it while it is idle: it takes messages
    /// at its next prompt or turn, or its own read.
    WaitsForPrompt,
    /// A current receiver; `receiving` once a receipt from this process
    /// says one was taken.
    Ready { receiving: bool },
    /// A current receiver holding its next input while the person types in
    /// its terminal ([`crate::typing`]); it takes it once they stop.
    HeldForTyping,
}

impl State {
    /// The words for it, the same on every surface.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ended => "Session ended",
            Self::ProviderBlocked(kind) => kind.label(),
            Self::Paused => "Not receiving messages",
            Self::Silent => "Not heard from recently",
            Self::Unreachable => "Can't receive messages",
            Self::AwaitsCheckIn => "Waits until it checks in",
            Self::WaitsForPrompt => "Messages may wait for its next prompt",
            Self::Ready { receiving: false } => "Ready for messages",
            Self::Ready { receiving: true } => "Receiving messages",
            Self::HeldForTyping => "Waiting for the person to stop typing",
        }
    }

    /// Whether a message sent now needs the sender's attention: anything
    /// but a ready receiver, or one that takes it once the person stops
    /// typing.
    pub fn needs_attention(self) -> bool {
        !matches!(self, Self::Ready { .. } | Self::HeldForTyping)
    }
}

/// The one thing that would get a session taking messages. Exactly one
/// per state and runtime ([`decide`]); a command or a session id rides
/// with it so every surface can name the same one.
///
/// On the wire it is `{"kind": "<snake_case>", ...}`; a kind this build
/// does not know, or a payload it cannot use, reads as [`Fix::Other`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Fix {
    /// Resume the Claude Code conversation with the AgentDocker channel:
    /// the whole command to paste ([`crate::send_readiness::claude_resume_command`]).
    Reconnect { command: String },
    /// Write the runtime's AgentDocker setup (`agentdocker setup
    /// <runtime>`), then restart the session so it loads it.
    SetupThenReconnect,
    /// Resume the Codex conversation from its project folder.
    CodexResume { session: String },
    /// Start a Codex from Launch agent, which takes messages live.
    LaunchLive,
    /// Resume delivery once the provider is back.
    WaitForProvider,
    /// Review the paused delivery before reconnecting it.
    ReviewDelivery,
    /// Nothing can wake it: the agent's own read of its inbox takes them.
    ReadInbox,
    /// It takes them when it checks in; nothing to do.
    WaitForCheckIn,
    /// Nothing to do, or nothing here can do it.
    None,
    /// A fix from a newer daemon.
    Other,
}

impl<'de> Deserialize<'de> for Fix {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Tolerant: a newer kind, or a payload this build cannot show
        // safely, never turns a committed send into a decode failure.
        let value = serde_json::Value::deserialize(deserializer)?;
        let field = |name: &str| {
            value
                .get(name)
                .and_then(serde_json::Value::as_str)
                .filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
                .map(str::to_owned)
        };
        Ok(
            match value.get("kind").and_then(serde_json::Value::as_str) {
                Some("reconnect") => {
                    field("command").map_or(Self::Other, |command| Self::Reconnect { command })
                }
                Some("setup_then_reconnect") => Self::SetupThenReconnect,
                Some("codex_resume") => {
                    field("session").map_or(Self::Other, |session| Self::CodexResume { session })
                }
                Some("launch_live") => Self::LaunchLive,
                Some("wait_for_provider") => Self::WaitForProvider,
                Some("review_delivery") => Self::ReviewDelivery,
                Some("read_inbox") => Self::ReadInbox,
                Some("wait_for_check_in") => Self::WaitForCheckIn,
                Some("none") => Self::None,
                _ => Self::Other,
            },
        )
    }
}

/// The decision about one agent: where it stands and the one fix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub state: State,
    pub fix: Fix,
}

/// Decide whether `agent` can receive input now and what one thing would
/// change that. `provider_block` is the kind of the provider block that
/// affects it, if any ([`provider_issue`]).
pub fn decide(
    agent: &AgentRecord,
    now: DateTime<Utc>,
    provider_block: Option<ProviderIssueKind>,
) -> Decision {
    let state = state(agent, now, provider_block);
    Decision {
        fix: fix(agent, state),
        state,
    }
}

/// [`decide`], with the provider block looked up among `agents` — every
/// record that could hold one, ended ones and other projects' included.
pub fn decide_among<'a>(
    agent: &AgentRecord,
    agents: impl IntoIterator<Item = &'a AgentRecord>,
    now: DateTime<Utc>,
) -> Decision {
    decide(agent, now, provider_issue(agent, agents))
}

/// The kind of the provider block among `agents` that affects `agent`.
pub fn provider_issue<'a>(
    agent: &AgentRecord,
    agents: impl IntoIterator<Item = &'a AgentRecord>,
) -> Option<ProviderIssueKind> {
    crate::provider_block(agent, agents)
        .and_then(|(_, state)| state.issue.as_ref())
        .map(|issue| issue.kind)
}

fn state(
    agent: &AgentRecord,
    now: DateTime<Utc>,
    provider_block: Option<ProviderIssueKind>,
) -> State {
    if !agent.status.is_live() {
        return State::Ended;
    }
    // The person reads in the app; nothing about a provider applies.
    if agent.spec.runtime == HUMAN_RUNTIME {
        return State::Ready { receiving: false };
    }
    if let Some(kind) = provider_block {
        return State::ProviderBlocked(kind);
    }
    match InputReadiness::for_agent(agent, now) {
        InputReadiness::SessionEnded => State::Ended,
        InputReadiness::Paused => State::Paused,
        InputReadiness::Stale => State::Silent,
        InputReadiness::Unreachable => State::Unreachable,
        // No process here whose receiver could be waited for: it reads
        // when it is next used, not at a prompt of ours.
        InputReadiness::Unverified if agent.via_connector() || agent.session_absent(now) => {
            State::AwaitsCheckIn
        }
        InputReadiness::Unverified => State::WaitsForPrompt,
        // A ready receiver that is holding its input for the person's
        // typing says so: what was sent waits on them, not on it.
        InputReadiness::AwaitingFirstReceipt | InputReadiness::Verified
            if agent.input_delivery.as_ref().is_some_and(|delivery| {
                delivery
                    .held_for_typing(agent.process_started_at, now)
                    .is_some()
            }) =>
        {
            State::HeldForTyping
        }
        InputReadiness::AwaitingFirstReceipt => State::Ready { receiving: false },
        InputReadiness::Verified => State::Ready { receiving: true },
    }
}

fn fix(agent: &AgentRecord, state: State) -> Fix {
    use crate::identity::{plain_session_id, relaunches_here, resumes_from_a_terminal};
    // A resume command only for a record a resumed terminal comes back
    // under: anything else would be a fresh record beside its queue. A
    // Claude session AgentDocker supervised comes back through the daemon
    // instead (`agentdocker reconnect`, Reconnect here in the app).
    let reconnect = || {
        if resumes_from_a_terminal(agent) {
            crate::send_readiness::claude_resume_command(agent, cfg!(windows))
                .map(|command| Fix::Reconnect { command })
        } else if relaunches_here(agent) && plain_session_id(agent.id.as_str()) {
            Some(Fix::Reconnect {
                command: format!("agentdocker reconnect {}", agent.id),
            })
        } else {
            None
        }
    };
    // Codex has no relaunch through the daemon: a supervised or bound
    // record gets no command.
    let codex_resume = || {
        resumes_from_a_terminal(agent)
            .then(|| crate::send_readiness::session_word(agent))
            .flatten()
            .map(|session| Fix::CodexResume {
                session: session.to_owned(),
            })
    };
    let runtime = agent.spec.runtime.as_str();
    match state {
        State::Ready { .. } | State::HeldForTyping => Fix::None,
        State::ProviderBlocked(_) => Fix::WaitForProvider,
        State::Paused => Fix::ReviewDelivery,
        State::AwaitsCheckIn => Fix::WaitForCheckIn,
        // Back under its own record with its queue, or nothing here can.
        State::Ended => match runtime {
            "claude-code" => reconnect(),
            "codex" => codex_resume(),
            _ => None,
        }
        .unwrap_or(Fix::None),
        // Nothing of it reached AgentDocker. For Claude the resume command
        // alone fails: without setup there is no MCP entry to load.
        State::Unreachable => match runtime {
            "claude-code" | "opencode" => Fix::SetupThenReconnect,
            "codex" => codex_resume().unwrap_or(Fix::LaunchLive),
            _ => Fix::ReadInbox,
        },
        // In touch, nothing wakes it. A Claude session without a command
        // to write never reported its conversation: its hooks are missing.
        State::WaitsForPrompt | State::Silent => match runtime {
            "claude-code" => reconnect().unwrap_or(Fix::SetupThenReconnect),
            "codex" => Fix::LaunchLive,
            // Its plugin wakes it and reports no receiver to wait for.
            "opencode" => Fix::None,
            _ => Fix::ReadInbox,
        },
    }
}

/// How a runtime is named in a sentence about its session.
fn runtime_name(runtime: &str) -> &str {
    match runtime {
        "claude-code" => "Claude",
        "codex" => "Codex",
        "opencode" => "OpenCode",
        other => other,
    }
}

impl Decision {
    /// The fix in a few words, beside the command or the button that does
    /// it. The long form, for somebody reading a send result, is
    /// [`Self::guidance`].
    pub fn hint(&self) -> &'static str {
        let live = self.state != State::Ended;
        match (&self.fix, self.state) {
            (Fix::Reconnect { .. }, _) if live => "Quit it, then run this in a terminal.",
            (Fix::Reconnect { .. }, _) => "Run this in a terminal to bring it back.",
            (Fix::SetupThenReconnect, _) => "Run this, then restart it.",
            (Fix::CodexResume { .. }, _) => "Run this in its project folder.",
            (Fix::LaunchLive, State::Unreachable) => {
                "Nothing of it reached AgentDocker; start a live one."
            }
            (Fix::LaunchLive, _) => "It takes messages at its next prompt.",
            (Fix::WaitForProvider, _) => "Resume delivery once the provider is back.",
            (Fix::ReviewDelivery, _) => "Review its delivery before reconnecting it.",
            (Fix::ReadInbox, _) => "It takes messages when it reads its inbox.",
            (Fix::WaitForCheckIn, _) => "It takes messages when it checks in.",
            (Fix::None, State::Ended) => "Its messages wait until it is resumed.",
            (Fix::None, State::Ready { .. }) => "It takes messages now.",
            (Fix::None, State::HeldForTyping) => "It takes them once the person stops typing.",
            (Fix::None, _) => "Its plugin wakes it; without it, at its next turn.",
            (Fix::Other, _) => "Check it in AgentDocker before sending again.",
        }
    }

    /// The command that does the fix, exactly as it is pasted, when the
    /// fix is one: the Claude resume, the setup, the Codex resume.
    pub fn command(&self, runtime: &str) -> Option<String> {
        match &self.fix {
            Fix::Reconnect { command } => Some(command.clone()),
            Fix::SetupThenReconnect => Some(format!("agentdocker setup {runtime}")),
            Fix::CodexResume { session } => Some(format!("codex resume {session}")),
            _ => None,
        }
    }

    /// Why it cannot take messages now and the one fix, in full.
    pub fn guidance(&self, runtime: &str, agent: &AgentId) -> String {
        let why = why(self.state, runtime);
        let id = agent.short();
        let fix = match &self.fix {
            Fix::Reconnect { command } if self.state == State::Ended => format!(
                "Bring it back with the AgentDocker channel by running this in a terminal: {command}. Complete Claude's channel consent when it asks."
            ),
            // The pasted resume loads the user's own MCP entry; the
            // daemon's relaunch supplies its own.
            Fix::Reconnect { command } if command.starts_with("agentdocker ") => format!(
                "After saving work and exiting it, run this in a terminal: {command}. Complete Claude's channel consent when it asks."
            ),
            Fix::Reconnect { command } => format!(
                "After saving work and exiting it, run this in a terminal: {command}. Complete Claude's channel consent; AgentDocker's MCP entry must include --claude-channel."
            ),
            Fix::SetupThenReconnect if runtime == "claude-code" => "Run agentdocker setup claude-code, then exit it and resume it from its project folder with the AgentDocker channel, completing Claude's channel consent.".into(),
            Fix::SetupThenReconnect if runtime == "opencode" => "Run agentdocker setup opencode, then restart OpenCode so it loads AgentDocker's plugin.".into(),
            Fix::SetupThenReconnect => format!("Run agentdocker setup {runtime}, then restart it."),
            Fix::CodexResume { session } => {
                format!("Resume it from its project folder with: codex resume {session}.")
            }
            Fix::LaunchLive if self.state == State::Unreachable => "A Codex started as plain `codex` names no conversation AgentDocker can find: start one from Launch agent, which takes them live.".into(),
            Fix::LaunchLive => "A Codex started from AgentDocker's Launch agent takes them live.".into(),
            Fix::WaitForProvider => "Check the provider limit or sign-in, then explicitly resume input after recovery.".into(),
            Fix::ReviewDelivery => "Review its delivery in AgentDocker before reconnecting it.".into(),
            Fix::ReadInbox => format!(
                "It takes them when the agent reads its inbox: agentdocker inbox --as {id}, watch --as {id} or the MCP read_inbox tool."
            ),
            Fix::WaitForCheckIn => "It takes them the next time it is used.".into(),
            Fix::None => String::new(),
            Fix::Other => "This client does not know the fix the daemon named; agentdocker ps --input-details shows it.".into(),
        };
        if fix.is_empty() {
            why
        } else {
            format!("{why} {fix}")
        }
    }
}

/// The first sentence of the guidance: where it stands, and that its
/// messages are kept. Never "may wait" for a session nothing will read.
pub(crate) fn why(state: State, runtime: &str) -> String {
    let name = runtime_name(runtime);
    match state {
        State::Ended => "This session has ended; its messages are kept until it takes them.".into(),
        State::ProviderBlocked(kind) => format!("{}: its messages are kept.", kind.label()),
        State::Paused => "Delivery to this session is paused; its messages are kept.".into(),
        State::Silent => "This session's receiver has gone quiet; its messages are kept.".into(),
        State::Unreachable => format!(
            "Nothing of this {name} session has reached AgentDocker, so it can't take messages; they are kept."
        ),
        State::AwaitsCheckIn => {
            "It reads messages only when it checks in; they are kept until then.".into()
        }
        State::WaitsForPrompt => match runtime {
            "claude-code" | "codex" => format!(
                "This {name} session takes messages at its next prompt; nothing wakes it while it is idle."
            ),
            "opencode" => "An OpenCode session with AgentDocker's plugin takes this at its next turn, and is woken for it when idle.".into(),
            other => format!("A {other} session cannot be woken by a message."),
        },
        State::Ready { .. } => "It takes messages now.".into(),
        State::HeldForTyping => "The person is typing in its terminal; what was sent waits until they stop, then goes in.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AdapterContact, AdapterKind, AgentSpec, AgentStatus, InputDelivery, RecipientReadiness,
        SendIssue,
    };

    const RUNTIMES: [&str; 5] = ["claude-code", "codex", "opencode", "custom", "gemini-cli"];
    const STATES: [&str; 9] = [
        "ended",
        "provider_blocked",
        "paused",
        "silent",
        "unreachable",
        "awaits_check_in",
        "waits_for_prompt",
        "ready",
        "held_for_typing",
    ];

    /// An absolute checkout on every platform.
    const CHECKOUT: &str = if cfg!(windows) {
        r"C:\work\orchard"
    } else {
        "/work/orchard"
    };

    fn born() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    /// A live session of `runtime` in `state`, with a conversation id and a
    /// folder when `known`. Returns it, the time to decide at, and the
    /// provider block to decide with.
    pub(crate) fn session(
        runtime: &str,
        state: &str,
        known: bool,
    ) -> (AgentRecord, DateTime<Utc>, Option<ProviderIssueKind>) {
        let birth = born();
        let mut agent = AgentRecord::new(
            AgentSpec {
                name: format!("{runtime}-1"),
                runtime: runtime.into(),
                ..Default::default()
            },
            false,
            birth,
        );
        agent.status = AgentStatus::Running;
        agent.started_at = Some(birth);
        agent.process_started_at = Some(birth);
        if known {
            agent.spec.workdir = Some(CHECKOUT.into());
            agent.project = Some(crate::ProjectRef::directory(CHECKOUT));
            agent
                .spec
                .labels
                .insert("session_id".into(), "1a2b3c4d-5e6f".into());
        }
        let contact = |agent: &mut AgentRecord| {
            agent.adapter_contacts.insert(
                AdapterKind::Hooks,
                AdapterContact {
                    process_started_at: birth,
                    observed_at: birth,
                },
            );
        };
        let delivery = |paused: bool, reported_at| InputDelivery {
            process_started_at: birth,
            paused,
            pause_reason: None,
            reported_at,
            received: None,
            received_at: None,
            typing_until: None,
        };
        let now = birth + chrono::Duration::minutes(5);
        let mut block = None;
        match state {
            "ended" => agent.status = AgentStatus::Exited { code: Some(0) },
            "provider_blocked" => {
                agent.input_delivery = Some(delivery(false, now));
                block = Some(ProviderIssueKind::Usage);
            }
            "paused" => agent.input_delivery = Some(delivery(true, now)),
            "silent" => agent.input_delivery = Some(delivery(false, birth)),
            "unreachable" => {}
            "awaits_check_in" => {
                // A session known by its key alone, gone quiet for hours.
                contact(&mut agent);
                agent.process_started_at = None;
                agent.spec.labels.insert("session_id".into(), "1a2b".into());
                agent.session_contact = Some(crate::agent::SessionContact {
                    adapter: AdapterKind::Hooks,
                    observed_at: birth,
                });
                return (
                    agent,
                    birth + crate::agent::SESSION_PRESENCE + chrono::Duration::minutes(1),
                    None,
                );
            }
            "waits_for_prompt" => contact(&mut agent),
            "ready" => agent.input_delivery = Some(delivery(false, now)),
            "held_for_typing" => {
                let mut held = delivery(false, now);
                held.typing_until = Some(now + crate::typing::QUIET);
                agent.input_delivery = Some(held);
            }
            other => panic!("no such state {other}"),
        }
        (agent, now, block)
    }

    /// The table: for every state and runtime, the one fix — with and
    /// without a conversation id and folder to name. Unreachable Claude
    /// is set up first, because the resume command fails without the MCP
    /// entry setup writes.
    #[test]
    fn every_state_and_runtime_has_exactly_one_fix() {
        let command = |agent: &AgentRecord| Fix::Reconnect {
            command: crate::send_readiness::claude_resume_command(agent, cfg!(windows)).unwrap(),
        };
        let resume = || Fix::CodexResume {
            session: "1a2b3c4d-5e6f".into(),
        };
        for known in [true, false] {
            for runtime in RUNTIMES {
                for name in STATES {
                    let (agent, now, block) = session(runtime, name, known);
                    let decision = decide(&agent, now, block);
                    let expected_state = match name {
                        "ended" => State::Ended,
                        "provider_blocked" => State::ProviderBlocked(ProviderIssueKind::Usage),
                        "paused" => State::Paused,
                        "silent" => State::Silent,
                        "unreachable" => State::Unreachable,
                        "awaits_check_in" => State::AwaitsCheckIn,
                        "waits_for_prompt" => State::WaitsForPrompt,
                        "held_for_typing" => State::HeldForTyping,
                        _ => State::Ready { receiving: false },
                    };
                    assert_eq!(decision.state, expected_state, "{runtime} {name} {known}");
                    let expected = match (expected_state, runtime, known) {
                        (State::Ready { .. } | State::HeldForTyping, ..) => Fix::None,
                        (State::ProviderBlocked(_), ..) => Fix::WaitForProvider,
                        (State::Paused, ..) => Fix::ReviewDelivery,
                        (State::AwaitsCheckIn, ..) => Fix::WaitForCheckIn,
                        (State::Ended, "claude-code", true) => command(&agent),
                        (State::Ended, "codex", true) => resume(),
                        (State::Ended, ..) => Fix::None,
                        (State::Unreachable, "claude-code" | "opencode", _) => {
                            Fix::SetupThenReconnect
                        }
                        (State::Unreachable, "codex", true) => resume(),
                        (State::Unreachable, "codex", false) => Fix::LaunchLive,
                        (State::Unreachable, ..) => Fix::ReadInbox,
                        (_, "claude-code", true) => command(&agent),
                        (_, "claude-code", false) => Fix::SetupThenReconnect,
                        (_, "codex", _) => Fix::LaunchLive,
                        (_, "opencode", _) => Fix::None,
                        _ => Fix::ReadInbox,
                    };
                    assert_eq!(decision.fix, expected, "{runtime} {name} known={known}");
                    // One fix, said one way: the hint is one short line,
                    // and the guidance names the same command.
                    let hint = decision.hint();
                    assert!(hint.len() <= 60 && !hint.contains('\n'), "{hint}");
                    let guidance = decision.guidance(runtime, &agent.id);
                    if let Some(command) = decision.command(runtime) {
                        assert!(guidance.contains(&command), "{runtime} {name}: {guidance}");
                    }
                    // Never "may wait" for a session nothing will read, and
                    // never two ways out.
                    assert!(!guidance.contains(", or "), "{runtime} {name}: {guidance}");
                    if decision.state == State::Unreachable {
                        assert!(!guidance.contains("may wait"), "{guidance}");
                    }
                }
            }
        }
        // A browser agent from the connector waits for its check-in too.
        let (mut browser, now, _) = session("claude-browser", "waits_for_prompt", false);
        browser.process_started_at = None;
        browser
            .spec
            .labels
            .insert(crate::agent::CONNECTOR_LABEL.into(), "true".into());
        assert_eq!(
            decide(&browser, now, None),
            Decision {
                state: State::AwaitsCheckIn,
                fix: Fix::WaitForCheckIn
            }
        );
        // The person reads in the app: nothing to fix, whatever the block.
        let (mut person, now, _) = session(HUMAN_RUNTIME, "unreachable", false);
        assert_eq!(
            decide(&person, now, Some(ProviderIssueKind::Rate)).fix,
            Fix::None
        );
        person.status = AgentStatus::Exited { code: None };
        assert_eq!(decide(&person, now, None).state, State::Ended);
    }

    /// A send result carries the decision as its issue and its fix; read
    /// back from the wire it says the same label, hint and guidance as the
    /// decision it came from, so the CLI, the MCP tools and the app, which
    /// read the wire, say what `ps --input-details` and the app's panels,
    /// which decide locally, say.
    #[test]
    fn the_wire_says_what_the_decision_says() {
        for known in [true, false] {
            for runtime in RUNTIMES {
                for name in STATES {
                    let (agent, now, block) = session(runtime, name, known);
                    let decision = decide(&agent, now, block);
                    let Some(readiness) = RecipientReadiness::for_agent(&agent, now, block) else {
                        assert!(!decision.state.needs_attention(), "{runtime} {name}");
                        continue;
                    };
                    let wire: RecipientReadiness =
                        serde_json::from_value(serde_json::to_value(&readiness).unwrap()).unwrap();
                    assert_eq!(wire.decision(), Some(decision.clone()), "{runtime} {name}");
                    assert_eq!(wire.label(), decision.state.label());
                    assert_eq!(wire.hint(), decision.hint());
                    assert_eq!(wire.guidance(), decision.guidance(runtime, &agent.id));
                }
            }
        }
    }

    /// A resume command is offered only for a record a resumed terminal
    /// comes back under. A session AgentDocker supervised, or one an input
    /// binding holds, keeps its queue from a plain resume: a Claude one is
    /// brought back through the daemon, a Codex one has no command.
    #[test]
    fn an_ended_session_a_terminal_cannot_take_back_is_not_offered_its_resume() {
        let binding = |agent: &AgentRecord| crate::InputBinding {
            provider: crate::ProviderGeneration {
                process: crate::ProcessIdentity {
                    pid: 8,
                    started_at: born(),
                },
                session: "1a2b3c4d-5e6f".into(),
                profile: "default".into(),
            },
            controller: crate::ProcessIdentity {
                pid: 9,
                started_at: born(),
            },
            controller_since: agent.created_at,
            token_sha256: "digest".into(),
            bound_at: agent.created_at,
            controller_generations: 1,
            uncertain: vec!["queued".to_owned().into()],
            launch: None,
            restart: Default::default(),
        };
        for state in ["ended", "unreachable"] {
            let (plain, now, _) = session("codex", state, true);
            let mut managed = plain.clone();
            managed.managed = true;
            let mut bound = plain.clone();
            bound.input_binding = Some(binding(&plain));
            let resume = Fix::CodexResume {
                session: "1a2b3c4d-5e6f".into(),
            };
            let none = if state == "ended" {
                Fix::None
            } else {
                Fix::LaunchLive
            };
            assert_eq!(decide(&plain, now, None).fix, resume, "{state}");
            assert_eq!(decide(&managed, now, None).fix, none, "{state} managed");
            assert_eq!(decide(&bound, now, None).fix, none, "{state} bound");
        }
        let (plain, now, _) = session("claude-code", "ended", true);
        let mut managed = plain.clone();
        managed.managed = true;
        managed.spec.tty = true;
        managed.id = "0123456789abcdef0123456789abcdef".into();
        assert_eq!(
            decide(&managed, now, None).fix,
            Fix::Reconnect {
                command: "agentdocker reconnect 0123456789abcdef0123456789abcdef".into()
            },
            "back under its own record through the daemon"
        );
        let mut bound = plain.clone();
        bound.input_binding = Some(binding(&plain));
        assert_eq!(decide(&bound, now, None).fix, Fix::None);
        let mut odd = managed.clone();
        odd.id = "id; rm".into();
        assert_eq!(decide(&odd, now, None).fix, Fix::None, "never shell syntax");
        // Live, the same command waits for it to be quit.
        let (mut waiting, now, _) = session("claude-code", "waits_for_prompt", true);
        waiting.managed = true;
        waiting.id = managed.id.clone();
        let decision = decide(&waiting, now, None);
        assert_eq!(decision.fix, decide(&managed, now, None).fix);
        assert_eq!(decision.hint(), "Quit it, then run this in a terminal.");
        assert!(
            !decision
                .guidance("claude-code", &waiting.id)
                .contains("--claude-channel"),
            "the daemon's relaunch brings its own MCP entry"
        );
    }

    /// A hold for the person's typing shows only while it lasts, only on
    /// a receiver that is otherwise ready, and asks nothing of the sender.
    #[test]
    fn a_hold_for_typing_shows_while_it_lasts_and_yields_to_what_stands_in_the_way() {
        let (held, now, _) = session("codex", "held_for_typing", true);
        let until = now + crate::typing::QUIET;
        assert_eq!(decide(&held, now, None).state, State::HeldForTyping);
        assert_eq!(
            decide(&held, until, None).state,
            State::Ready { receiving: false },
            "it lapses by itself"
        );
        assert_eq!(
            decide(&held, now, Some(ProviderIssueKind::Usage)).state,
            State::ProviderBlocked(ProviderIssueKind::Usage)
        );
        let mut paused = held.clone();
        paused.input_delivery.as_mut().unwrap().paused = true;
        assert_eq!(decide(&paused, now, None).state, State::Paused);
        assert!(RecipientReadiness::for_agent(&held, now, None).is_none());
        assert_eq!(
            State::HeldForTyping.label(),
            "Waiting for the person to stop typing"
        );
    }

    /// An absent keyed session is not "unreachable" and not "next prompt":
    /// it is away, and reads when it is back; present, it waits for its
    /// next prompt like any session in touch.
    #[test]
    fn a_keyed_session_waits_for_its_check_in_only_while_absent() {
        let (agent, later, _) = session("codex", "awaits_check_in", true);
        assert!(agent.session_absent(later));
        assert_eq!(decide(&agent, later, None).state, State::AwaitsCheckIn);
        let soon = born() + chrono::Duration::minutes(5);
        assert!(!agent.session_absent(soon));
        assert_eq!(decide(&agent, soon, None).state, State::WaitsForPrompt);
        let readiness = RecipientReadiness::for_agent(&agent, later, None).unwrap();
        assert_eq!(
            readiness.issue,
            SendIssue::NoReceiver,
            "the wire kind stays"
        );
        assert_eq!(readiness.label(), "Waits until it checks in");
    }

    #[test]
    fn a_fix_from_a_newer_daemon_or_one_unsafe_to_show_reads_as_other() {
        for (wire, fix) in [
            (serde_json::json!({"kind": "launch_live"}), Fix::LaunchLive),
            (
                serde_json::json!({"kind": "reconnect", "command": "cd '/w' && claude"}),
                Fix::Reconnect {
                    command: "cd '/w' && claude".into(),
                },
            ),
            (
                serde_json::json!({"kind": "future_fix", "detail": [1]}),
                Fix::Other,
            ),
            (serde_json::json!({"kind": "reconnect"}), Fix::Other),
            (
                serde_json::json!({"kind": "reconnect", "command": "claude\u{1b}[2J"}),
                Fix::Other,
            ),
            (
                serde_json::json!({"kind": "codex_resume", "session": 7}),
                Fix::Other,
            ),
            (serde_json::json!("reconnect"), Fix::Other),
        ] {
            assert_eq!(serde_json::from_value::<Fix>(wire).unwrap(), fix);
        }
        assert_eq!(
            serde_json::to_value(Fix::CodexResume {
                session: "abc".into()
            })
            .unwrap(),
            serde_json::json!({"kind": "codex_resume", "session": "abc"})
        );
        assert_eq!(
            serde_json::to_value(Fix::WaitForCheckIn).unwrap(),
            serde_json::json!({"kind": "wait_for_check_in"})
        );
    }
}
