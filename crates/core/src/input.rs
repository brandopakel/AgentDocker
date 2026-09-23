//! Durable delivery evidence. Queueing, provider receipt and task completion
//! are separate facts; neither a socket write nor an empty inbox is a receipt.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::MessageId;

/// Evidence about a session's input receiver, not receipt of any particular
/// message or proof that its provider is available. Provider limits are separate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputReadiness {
    SessionEnded,
    Unverified,
    Paused,
    Stale,
    AwaitingFirstReceipt,
    Verified,
}

impl InputReadiness {
    pub fn for_agent(agent: &crate::AgentRecord, now: DateTime<Utc>) -> Self {
        if !agent.status.is_live() {
            return Self::SessionEnded;
        }
        let Some(delivery) = agent.input_delivery.as_ref() else {
            return Self::Unverified;
        };
        if delivery.paused_for(agent.process_started_at) {
            return Self::Paused;
        }
        if !delivery.current_for(agent.process_started_at, now) {
            return Self::Stale;
        }
        if delivery.received_for(agent.process_started_at, now) {
            Self::Verified
        } else {
            Self::AwaitingFirstReceipt
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::SessionEnded => "Session ended",
            Self::Unverified => "Messages may wait for its next prompt",
            Self::Paused => "Not receiving messages",
            Self::Stale => "Not heard from recently",
            Self::AwaitingFirstReceipt => "Ready for messages",
            Self::Verified => "Receiving messages",
        }
    }
}

/// Why the daemon paused an agent's input delivery. The daemon writes these
/// as `pause_reason`; clients that explain a pause in their own words match
/// on them, so they live here rather than as literals on either side.
pub const PAUSE_CONTROLLER_ENDED: &str = "the bound controller ended";
pub const PAUSE_CONTROLLER_RESTART_FAILED: &str = "the bound controller could not be restarted";
pub const PAUSE_RECEIVER_UPGRADING: &str = "the input receiver is being upgraded";

/// Contact with a particular adapter is separate from generic agent activity.
/// These observations contain no provider configuration or message contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterKind {
    Mcp,
    Hooks,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterContact {
    pub process_started_at: DateTime<Utc>,
    pub observed_at: DateTime<Utc>,
}

impl AdapterContact {
    pub fn current_for(&self, generation: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
        generation == Some(self.process_started_at)
            && self.observed_at >= self.process_started_at
            && self.observed_at <= now
            && now - self.observed_at < chrono::Duration::minutes(5)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputReceipt {
    Codex {
        thread: String,
        turn: String,
        item: String,
    },
    ClaudeChannel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceivedInput {
    pub messages: Vec<MessageId>,
    pub receipt: InputReceipt,
}

impl ReceivedInput {
    pub fn valid(&self) -> bool {
        fn id(value: &str) -> bool {
            !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
        }
        !self.messages.is_empty()
            && self.messages.len() <= 1000
            && self.messages.iter().all(|message| id(message.as_str()))
            && self
                .messages
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                == self.messages.len()
            && match &self.receipt {
                InputReceipt::Codex { thread, turn, item } => {
                    self.messages.len() == 1 && id(thread) && id(turn) && id(item)
                }
                InputReceipt::ClaudeChannel => true,
            }
    }
}

/// One process, exactly: a pid and the moment it started, together, so a
/// recycled pid is never mistaken for the process that bound or served.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub started_at: DateTime<Utc>,
}

/// The provider session an external controller feeds: which process,
/// which conversation, which configuration. Provider-neutral, and none of
/// it is secret: a profile is a name or a digest, never its contents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderGeneration {
    pub process: ProcessIdentity,
    /// The thread, session or conversation the controller feeds.
    pub session: String,
    /// The provider profile in use, by name or digest.
    pub profile: String,
}

impl ProviderGeneration {
    pub fn valid(&self) -> bool {
        let id = |value: &str| {
            !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        };
        self.process.pid > 0 && id(&self.session) && id(&self.profile)
    }
}

/// How the daemon starts a controller again once it has ended: the exact
/// command it was started with, changed only by an explicit receiver upgrade. An idle
/// provider has no hook left to restart its receiver, so without this a
/// controller that was killed stays bound and dead until somebody types.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerLaunch {
    pub executable: std::path::PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    pub cwd: std::path::PathBuf,
    /// Added to the daemon's own environment. The descriptor is kept on
    /// the agent record, in the open: a secret belongs in a file, not here.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
}

impl ControllerLaunch {
    pub fn valid(&self) -> bool {
        let text = |value: &str| value.len() <= 4096 && !value.contains('\0');
        self.executable.is_absolute()
            && self.cwd.is_absolute()
            && self.args.len() <= 64
            && self.args.iter().all(|arg| text(arg))
            && self.env.len() <= 64
            && self.env.iter().all(|(key, value)| {
                !key.is_empty() && !key.contains('=') && text(key) && text(value)
            })
    }
}

/// Launches per episode before the daemon gives up. An episode ends when a
/// controller stays bound for [`CONTROLLER_STABLE`].
pub const CONTROLLER_RESTARTS: u32 = 5;
/// A controller bound this long counts as recovered: the next end starts
/// the count over.
pub const CONTROLLER_STABLE: chrono::Duration = chrono::Duration::seconds(60);
/// A launched controller has this long to bind before it is told to stop,
/// and this much more before it is killed.
pub const CONTROLLER_BIND_GRACE: chrono::Duration = chrono::Duration::seconds(30);
pub const CONTROLLER_KILL_AFTER: chrono::Duration = chrono::Duration::seconds(5);

/// How long after the last end the given launch waits: the first one is
/// immediate, then 2, 4, 8, 16 seconds.
pub fn controller_backoff(attempt: u32) -> chrono::Duration {
    match attempt {
        0 | 1 => chrono::Duration::zero(),
        n => chrono::Duration::seconds(2i64.pow((n - 1).min(6))),
    }
}

/// The daemon's record of restarting a controller, kept on the binding so
/// a replaced daemon continues the count rather than starting over.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ControllerRestart {
    /// Durable intent to stop the bound receiver for an executable upgrade.
    /// Survives a coordinator crash after the new descriptor committed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upgrade_requested_at: Option<DateTime<Utc>>,
    /// Launches since a controller last stayed bound for [`CONTROLLER_STABLE`].
    pub attempts: u32,
    /// The process the daemon launched last, until it binds or ends.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launched: Option<ProcessIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launched_at: Option<DateTime<Utc>>,
    /// When the controller, bound or launched, was last found ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    /// The daemon stopped launching after [`CONTROLLER_RESTARTS`] attempts.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub exhausted: bool,
}

/// What the daemon does about a binding's controller at one look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControllerStep {
    /// The bound receiver must stop for an explicitly committed upgrade.
    Upgrade { kill: bool },
    /// Alive, starting, waiting out a backoff, unmanaged, or given up.
    Keep,
    /// The bound or launched controller is gone and nobody has noted it.
    Ended,
    /// Start the descriptor: this many launches in the episode, counting it.
    Launch { attempt: u32 },
    /// The launched process is alive but has not bound within its grace:
    /// tell it to stop, then kill it.
    Terminate { kill: bool },
    /// The episode's launches are used up.
    Exhausted,
}

/// Who consumes an agent's queued input: one controller process, bound to
/// one provider generation. While a binding stands, legacy readers are
/// answered `input_owned` and only the controller's token-bearing reads
/// take messages. The token itself is never stored, only its digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputBinding {
    pub provider: ProviderGeneration,
    pub controller: ProcessIdentity,
    /// When the current controller bound: the first bind, or its resume.
    #[serde(default)]
    pub controller_since: DateTime<Utc>,
    pub token_sha256: String,
    pub bound_at: DateTime<Utc>,
    /// How many controller processes have held this binding: one at the
    /// first bind, one more for each controller that resumed it.
    pub controller_generations: u32,
    /// Queued messages a legacy reader had already been offered when the
    /// binding was made. They are still delivered to the controller,
    /// flagged, so it reconciles against the provider before enqueueing
    /// them; a legacy acknowledgement is still accepted for exactly these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncertain: Vec<MessageId>,
    /// How to start the controller again, when the controller said so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<ControllerLaunch>,
    #[serde(default, skip_serializing_if = "ControllerRestart::is_idle")]
    pub restart: ControllerRestart,
}

impl ControllerRestart {
    fn is_idle(&self) -> bool {
        *self == Self::default()
    }
}

impl InputBinding {
    /// Whether a presented token's digest is the one this binding keeps.
    /// The digest is computed by whoever holds the token; core keeps no
    /// hashing of its own.
    pub fn accepts_digest(&self, digest: &str) -> bool {
        !self.token_sha256.is_empty() && self.token_sha256 == digest
    }

    /// One look at the controller. `alive` answers for the bound
    /// controller, a launched one and the provider process; a live process
    /// always keeps its place, and a provider that is gone gets no
    /// controller: the daemon restarts receivers, never sessions.
    pub fn controller_step(
        &self,
        now: DateTime<Utc>,
        alive: impl Fn(&ProcessIdentity) -> bool,
    ) -> ControllerStep {
        if alive(&self.controller) {
            return match self.restart.upgrade_requested_at {
                Some(since) => ControllerStep::Upgrade {
                    kill: now - since >= CONTROLLER_KILL_AFTER,
                },
                None => ControllerStep::Keep,
            };
        }
        let restart = &self.restart;
        if let Some(launched) = &restart.launched {
            if !alive(launched) {
                return ControllerStep::Ended;
            }
            let since = restart.launched_at.unwrap_or(now);
            if now - since < CONTROLLER_BIND_GRACE {
                return ControllerStep::Keep;
            }
            return ControllerStep::Terminate {
                kill: now - since >= CONTROLLER_BIND_GRACE + CONTROLLER_KILL_AFTER,
            };
        }
        let Some(ended_at) = restart.ended_at else {
            return ControllerStep::Ended;
        };
        if self.launch.is_none() || restart.exhausted || !alive(&self.provider.process) {
            return ControllerStep::Keep;
        }
        if restart.attempts >= CONTROLLER_RESTARTS {
            return ControllerStep::Exhausted;
        }
        let attempt = restart.attempts + 1;
        if now < ended_at + controller_backoff(attempt) {
            return ControllerStep::Keep;
        }
        ControllerStep::Launch { attempt }
    }

    /// Note that the controller, or the process launched to replace it,
    /// has ended. A controller that had stayed bound long enough starts
    /// the episode over.
    pub fn note_controller_ended(&mut self, now: DateTime<Utc>) {
        self.restart.upgrade_requested_at = None;
        if self.restart.launched.is_none() && now - self.controller_since >= CONTROLLER_STABLE {
            self.restart.attempts = 0;
            self.restart.exhausted = false;
        }
        self.restart.launched = None;
        self.restart.launched_at = None;
        self.restart.ended_at = Some(now);
    }

    /// Note a launch: the process started, or the attempt failed before
    /// it did, which counts the same and waits out the same backoff.
    pub fn note_launch(&mut self, launched: Option<ProcessIdentity>, now: DateTime<Utc>) {
        self.restart.attempts = self.restart.attempts.saturating_add(1);
        self.restart.launched_at = launched.is_some().then_some(now);
        self.restart.launched = launched;
        self.restart.ended_at = Some(now);
    }

    /// A controller bound: the one the daemon launched, which keeps the
    /// episode's count, or another one, which starts a new life.
    pub fn note_controller_bound(&mut self, controller: ProcessIdentity, now: DateTime<Utc>) {
        self.restart.upgrade_requested_at = None;
        if self.restart.launched.as_ref() != Some(&controller) {
            self.restart.attempts = 0;
            self.restart.exhausted = false;
        }
        self.restart.launched = None;
        self.restart.launched_at = None;
        self.restart.ended_at = None;
        self.controller = controller;
        self.controller_since = now;
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputReport {
    Ready,
    Received { input: ReceivedInput },
    Paused { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputDelivery {
    /// The process generation that made the most recent report.
    pub process_started_at: DateTime<Utc>,
    pub paused: bool,
    pub pause_reason: Option<String>,
    pub reported_at: DateTime<Utc>,
    /// The latest receipt remains historical evidence across restarts.
    pub received: Option<ReceivedInput>,
    pub received_at: Option<DateTime<Utc>>,
}

impl InputDelivery {
    pub fn paused_for(&self, process_started_at: Option<DateTime<Utc>>) -> bool {
        self.paused && process_started_at == Some(self.process_started_at)
    }

    /// A receiver refreshes its report while servicing input. An old receipt,
    /// a reused PID or a silent/stopped controller cannot establish readiness.
    pub fn current_for(&self, generation: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
        !self.paused
            && generation == Some(self.process_started_at)
            && self.reported_at >= self.process_started_at
            && self.reported_at <= now
            && now - self.reported_at < chrono::Duration::seconds(90)
    }

    pub fn received_for(&self, generation: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
        generation == Some(self.process_started_at)
            && self.received.is_some()
            && self
                .received_at
                .is_some_and(|at| at >= self.process_started_at && at <= now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receiver_evidence_is_independent_of_vendor_and_rechecked_after_restart_or_silence() {
        let birth = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        for runtime in ["codex", "claude-code", "gemini-cli", "cursor", "custom"] {
            let mut agent = crate::AgentRecord::new(
                crate::AgentSpec {
                    runtime: runtime.into(),
                    ..Default::default()
                },
                false,
                birth,
            );
            agent.status = crate::AgentStatus::Running;
            agent.process_started_at = Some(birth);
            for adapter in [AdapterKind::Mcp, AdapterKind::Hooks] {
                agent.adapter_contacts.insert(
                    adapter,
                    AdapterContact {
                        process_started_at: birth,
                        observed_at: birth,
                    },
                );
            }
            assert_eq!(
                InputReadiness::for_agent(&agent, birth),
                InputReadiness::Unverified
            );
            agent.input_delivery = Some(InputDelivery {
                process_started_at: birth,
                paused: false,
                pause_reason: None,
                reported_at: birth,
                received: None,
                received_at: None,
            });
            assert_eq!(
                InputReadiness::for_agent(&agent, birth),
                InputReadiness::AwaitingFirstReceipt
            );
            let delivery = agent.input_delivery.as_mut().unwrap();
            delivery.received = Some(ReceivedInput {
                messages: vec!["previous-message".to_owned().into()],
                receipt: InputReceipt::ClaudeChannel,
            });
            delivery.received_at = Some(birth);
            assert_eq!(
                InputReadiness::for_agent(&agent, birth),
                InputReadiness::Verified
            );
            assert_eq!(
                InputReadiness::for_agent(&agent, birth + chrono::Duration::seconds(90)),
                InputReadiness::Stale
            );
            agent.process_started_at = Some(birth + chrono::Duration::seconds(1));
            assert_eq!(
                InputReadiness::for_agent(&agent, birth),
                InputReadiness::Stale
            );
            agent.process_started_at = Some(birth);
            agent.input_delivery.as_mut().unwrap().paused = true;
            assert_eq!(
                InputReadiness::for_agent(&agent, birth),
                InputReadiness::Paused
            );
            agent.status = crate::AgentStatus::Exited { code: Some(0) };
            assert_eq!(
                InputReadiness::for_agent(&agent, birth),
                InputReadiness::SessionEnded
            );
        }
    }

    #[test]
    fn contact_and_receiver_readiness_require_fresh_generation_bound_evidence() {
        let birth = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let contact = AdapterContact {
            process_started_at: birth,
            observed_at: birth,
        };
        for age in [0, 299] {
            assert!(contact.current_for(Some(birth), birth + chrono::Duration::seconds(age)));
        }
        for age in [-1, 300] {
            assert!(!contact.current_for(Some(birth), birth + chrono::Duration::seconds(age)));
        }
        assert!(!contact.current_for(None, birth));
        assert!(!contact.current_for(Some(birth + chrono::Duration::seconds(1)), birth));
        let mut delivery = InputDelivery {
            process_started_at: birth,
            paused: false,
            pause_reason: None,
            reported_at: birth,
            received: None,
            received_at: None,
        };
        assert!(delivery.current_for(Some(birth), birth + chrono::Duration::seconds(89)));
        assert!(!delivery.current_for(Some(birth), birth + chrono::Duration::seconds(90)));
        assert!(!delivery.current_for(Some(birth), birth - chrono::Duration::seconds(1)));
        assert!(!delivery.current_for(None, birth));
        assert!(!delivery.received_for(Some(birth), birth));
        delivery.received = Some(ReceivedInput {
            messages: vec!["receipt".to_owned().into()],
            receipt: InputReceipt::ClaudeChannel,
        });
        delivery.received_at = Some(birth - chrono::Duration::seconds(1));
        assert!(
            !delivery.received_for(Some(birth), birth),
            "historical receipt is not current-process evidence"
        );
        delivery.received_at = Some(birth);
        assert!(delivery.received_for(Some(birth), birth));
        assert!(!delivery.received_for(Some(birth + chrono::Duration::seconds(1)), birth));
        assert!(!delivery.received_for(None, birth));
        delivery.paused = true;
        assert!(!delivery.current_for(Some(birth), birth));
        assert!(
            delivery.received_for(Some(birth), birth),
            "pausing retains the receipt as history"
        );
    }

    #[test]
    fn receipts_require_bounded_unique_ids_and_one_codex_item() {
        let mut input = ReceivedInput {
            messages: vec!["message".to_owned().into()],
            receipt: InputReceipt::Codex {
                thread: "thread".into(),
                turn: "turn".into(),
                item: "item".into(),
            },
        };
        assert!(input.valid());
        input.messages.push("another".to_owned().into());
        assert!(!input.valid(), "one Codex item cannot prove two inputs");
        input.receipt = InputReceipt::ClaudeChannel;
        assert!(input.valid());
        input.messages.push("message".to_owned().into());
        assert!(!input.valid());
        for bad in ["".to_owned(), "a".repeat(129), "line\nbreak".into()] {
            input.messages = vec![bad.into()];
            assert!(!input.valid());
        }
        input.messages = (0..1001).map(|i| i.to_string().into()).collect();
        assert!(!input.valid());
    }

    #[test]
    fn legacy_agent_and_activity_do_not_invent_delivery_evidence() {
        let record = crate::AgentRecord::new(
            crate::AgentSpec::default(),
            false,
            DateTime::from_timestamp(1, 0).unwrap(),
        );
        let mut value = serde_json::to_value(record).unwrap();
        value.as_object_mut().unwrap().remove("input_delivery");
        assert!(
            serde_json::from_value::<crate::AgentRecord>(value)
                .unwrap()
                .input_delivery
                .is_none()
        );
        let activity: crate::AgentActivity = serde_json::from_value(
            serde_json::json!({"agent":"id", "name":"agent", "activity":{"state":"unknown"}}),
        )
        .unwrap();
        assert_eq!(activity.queued_inputs, None);
    }

    fn process(pid: u32) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            started_at: DateTime::from_timestamp(1_700_000_000 + i64::from(pid), 0).unwrap(),
        }
    }

    fn binding(now: DateTime<Utc>, launch: bool) -> InputBinding {
        InputBinding {
            provider: ProviderGeneration {
                process: process(10),
                session: "thread".into(),
                profile: "/profiles/default".into(),
            },
            controller: process(20),
            controller_since: now,
            token_sha256: "digest".into(),
            bound_at: now,
            controller_generations: 1,
            uncertain: Vec::new(),
            // Absolute on every platform, Windows included.
            launch: launch.then(|| ControllerLaunch {
                executable: std::env::temp_dir().join("receiver"),
                args: vec!["--agent".into(), "a".into()],
                cwd: std::env::temp_dir(),
                env: Default::default(),
            }),
            restart: ControllerRestart::default(),
        }
    }

    #[test]
    fn a_controller_that_ended_is_restarted_with_backoff_a_bounded_number_of_times() {
        let start = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let mut binding = binding(start, true);
        let dead = |process: &ProcessIdentity| process.pid == 10;
        let all = |_: &ProcessIdentity| true;
        assert_eq!(binding.controller_step(start, all), ControllerStep::Keep);
        // First sight of the end is noted, not acted on.
        let t = start + chrono::Duration::seconds(5);
        assert_eq!(binding.controller_step(t, dead), ControllerStep::Ended);
        binding.note_controller_ended(t);
        assert_eq!(binding.restart.ended_at, Some(t));
        // The first launch is immediate; each next one waits longer.
        let mut t = t;
        for (attempt, launched) in (1..=CONTROLLER_RESTARTS).zip(100u32..) {
            let wait = controller_backoff(attempt);
            if wait > chrono::Duration::zero() {
                assert_eq!(
                    binding.controller_step(t + wait - chrono::Duration::seconds(1), dead),
                    ControllerStep::Keep,
                    "attempt {attempt} waits"
                );
            }
            t += wait;
            assert_eq!(
                binding.controller_step(t, dead),
                ControllerStep::Launch { attempt }
            );
            binding.note_launch(Some(process(launched)), t);
            assert_eq!(binding.restart.attempts, attempt);
            // A launched process that is alive holds its place; when it
            // ends without binding, that is an end like any other.
            let starting = move |p: &ProcessIdentity| p.pid == launched || p.pid == 10;
            assert_eq!(binding.controller_step(t, starting), ControllerStep::Keep);
            assert_eq!(
                binding.controller_step(t + CONTROLLER_BIND_GRACE, starting),
                ControllerStep::Terminate { kill: false }
            );
            assert_eq!(
                binding
                    .controller_step(t + CONTROLLER_BIND_GRACE + CONTROLLER_KILL_AFTER, starting),
                ControllerStep::Terminate { kill: true }
            );
            t += chrono::Duration::seconds(1);
            assert_eq!(binding.controller_step(t, dead), ControllerStep::Ended);
            binding.note_controller_ended(t);
        }
        assert_eq!(
            binding.controller_step(t + chrono::Duration::hours(1), dead),
            ControllerStep::Exhausted
        );
        binding.restart.exhausted = true;
        assert_eq!(
            binding.controller_step(t + chrono::Duration::hours(2), dead),
            ControllerStep::Keep
        );
        assert_eq!(controller_backoff(2), chrono::Duration::seconds(2));
        assert_eq!(controller_backoff(5), chrono::Duration::seconds(16));
        assert_eq!(controller_backoff(40), chrono::Duration::seconds(64));
    }

    #[test]
    fn a_launched_controller_that_binds_keeps_the_count_and_a_stable_one_resets_it() {
        let start = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let mut binding = binding(start, true);
        let dead = |process: &ProcessIdentity| process.pid == 10;
        binding.note_controller_ended(start);
        binding.note_launch(Some(process(100)), start);
        // The launched process binds: the episode goes on counting.
        binding.note_controller_bound(process(100), start + chrono::Duration::seconds(2));
        assert_eq!(binding.controller, process(100));
        assert_eq!(binding.restart.attempts, 1);
        assert_eq!(binding.restart.launched, None);
        assert_eq!(binding.restart.ended_at, None);
        // It ends within the stable window: attempt two waits its backoff.
        let t = start + chrono::Duration::seconds(30);
        assert_eq!(binding.controller_step(t, dead), ControllerStep::Ended);
        binding.note_controller_ended(t);
        assert_eq!(binding.restart.attempts, 1);
        assert_eq!(
            binding.controller_step(t + chrono::Duration::seconds(1), dead),
            ControllerStep::Keep
        );
        assert_eq!(
            binding.controller_step(t + chrono::Duration::seconds(2), dead),
            ControllerStep::Launch { attempt: 2 }
        );
        binding.note_launch(Some(process(101)), t + chrono::Duration::seconds(2));
        binding.note_controller_bound(process(101), t + chrono::Duration::seconds(3));
        // Bound for a minute: the next end starts a fresh episode.
        let t = t + chrono::Duration::seconds(3) + CONTROLLER_STABLE;
        binding.note_controller_ended(t);
        assert_eq!(binding.restart.attempts, 0);
        assert_eq!(
            binding.controller_step(t, dead),
            ControllerStep::Launch { attempt: 1 }
        );
        // A controller the daemon did not launch is a new life too, even
        // after the daemon gave up.
        binding.note_launch(None, t);
        binding.restart.attempts = CONTROLLER_RESTARTS;
        binding.restart.exhausted = true;
        binding.note_controller_bound(process(200), t + chrono::Duration::seconds(1));
        assert_eq!(binding.restart, ControllerRestart::default());
        assert_eq!(binding.controller_since, t + chrono::Duration::seconds(1));
    }

    #[test]
    fn restarts_need_a_descriptor_and_a_live_provider() {
        let start = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let nobody = |_: &ProcessIdentity| false;
        let provider_only = |p: &ProcessIdentity| p.pid == 10;
        // Unmanaged: the end is noted once, then nothing.
        let mut plain = binding(start, false);
        assert_eq!(plain.controller_step(start, nobody), ControllerStep::Ended);
        plain.note_controller_ended(start);
        assert_eq!(
            plain.controller_step(start + chrono::Duration::hours(1), provider_only),
            ControllerStep::Keep
        );
        // Managed, but the provider is gone: receivers are restarted, sessions never.
        let mut managed = binding(start, true);
        managed.note_controller_ended(start);
        assert_eq!(managed.controller_step(start, nobody), ControllerStep::Keep);
        assert_eq!(
            managed.controller_step(start, provider_only),
            ControllerStep::Launch { attempt: 1 }
        );
        let descriptor = managed.launch.clone().unwrap();
        assert!(descriptor.valid());
        assert!(
            !ControllerLaunch {
                executable: "receiver".into(),
                ..descriptor.clone()
            }
            .valid()
        );
        assert!(
            !ControllerLaunch {
                env: [("A=B".to_owned(), "x".to_owned())].into_iter().collect(),
                ..descriptor.clone()
            }
            .valid()
        );
        assert!(
            !ControllerLaunch {
                args: vec!["nul\0".into()],
                ..descriptor
            }
            .valid()
        );
        // The restart record only travels when there is something in it.
        let fresh = binding(start, false);
        let json = serde_json::to_value(&fresh).unwrap();
        assert!(json.get("launch").is_none());
        assert!(json.get("restart").is_none());
        let back: InputBinding = serde_json::from_value(json).unwrap();
        assert_eq!(back, fresh);
        let json = serde_json::to_value(&managed).unwrap();
        assert!(json.get("restart").is_some());
        let back: InputBinding = serde_json::from_value(json).unwrap();
        assert_eq!(back, managed);
    }
}
