//! Container engine selection and immutable build evidence, separate from agent runtime.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf, str::FromStr};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerEngine {
    Docker,
    Podman,
}
impl fmt::Display for ContainerEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Docker => "docker",
            Self::Podman => "podman",
        })
    }
}
impl FromStr for ContainerEngine {
    type Err = String;
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "docker" => Ok(Self::Docker),
            "podman" => Ok(Self::Podman),
            _ => Err("engine must be docker or podman".into()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageBuildSpec {
    pub engine: ContainerEngine,
    /// Docker context or Podman connection; absent uses that engine's configured default.
    #[serde(default)]
    pub connection: Option<String>,
    pub context: PathBuf,
    /// Relative to context; a regular file within the captured build inputs.
    pub recipe: PathBuf,
    #[serde(default = "build_timeout")]
    pub timeout_secs: u64,
}
fn build_timeout() -> u64 {
    600
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageBuild {
    pub id: String,
    pub spec: ImageBuildSpec,
    pub captured_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    /// Hash of every captured path, file byte, permission and symlink target.
    pub context_version: String,
    pub recipe_version: String,
    /// Immutable local image configuration identity, never a mutable tag.
    pub image_id: String,
    pub client_version: String,
    pub server_version: Option<String>,
    pub os: String,
    pub architecture: String,
    pub variant: Option<String>,
}

/// Portable build inputs; local build IDs and connection names remain audit metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageInputs {
    pub context_version: String,
    pub recipe_version: String,
    pub os: String,
    pub architecture: String,
    pub variant: Option<String>,
}
impl From<&ImageBuild> for ImageInputs {
    fn from(build: &ImageBuild) -> Self {
        Self {
            context_version: build.context_version.clone(),
            recipe_version: build.recipe_version.clone(),
            os: build.os.clone(),
            architecture: build.architecture.clone(),
            variant: build.variant.clone(),
        }
    }
}

/// Durable intent for one managed container. A replacement run gets a new identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerIntent {
    Run,
    Stop,
    Kill,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedContainer {
    #[serde(default)]
    pub inputs: Option<ImageInputs>,
    pub build: String,
    pub engine: ContainerEngine,
    pub connection: Option<String>,
    pub image_id: String,
    /// Unique engine name used only to recover a lost create response.
    pub name: String,
    /// Random ownership label, checked along with the agent and image identity.
    pub owner: String,
    pub id: Option<String>,
    pub intent: ContainerIntent,
    /// Written before invoking start, including when its response is lost.
    pub start_attempted: bool,
    /// Legacy records may already exist in the engine; new runs begin with false.
    #[serde(default = "legacy_create_attempted")]
    pub create_attempted: bool,
    pub last_error: Option<String>,
    #[serde(default)]
    pub options: ContainerRunOptions,
    #[serde(default)]
    pub workspace: Option<ContainerWorkspace>,
    /// Validation runners are killed after this durable deadline, including after recovery.
    #[serde(default)]
    pub deadline: Option<DateTime<Utc>>,
    /// The dead end the reconciler last refused at, kept apart from
    /// `last_error`, which any later failure overwrites. Set only by that
    /// refusal; cleared by an observed run or exit and by a stop or kill
    /// intent, and a new record starts without one. It is what readers
    /// show, never what the reconciler decides by. Absent from a record
    /// written before it, and an older daemon that rewrites the record
    /// drops it until the next refusal sets it again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dead_end: Option<ContainerDeadEnd>,
}

/// A dead end the reconciler refused a container at, as its record keeps
/// it for readers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerDeadEnd {
    StartUnknown,
    PrepareAbandoned,
    /// One a newer daemon recorded.
    #[serde(other)]
    Unknown,
}

impl ContainerDeadEnd {
    /// The dead end a phase is, if it is one.
    pub fn of(phase: ContainerPhase) -> Option<Self> {
        match phase {
            ContainerPhase::StartUnknown => Some(Self::StartUnknown),
            ContainerPhase::PrepareAbandoned => Some(Self::PrepareAbandoned),
            ContainerPhase::Prepared
            | ContainerPhase::Creating
            | ContainerPhase::Created
            | ContainerPhase::Starting
            | ContainerPhase::Running
            | ContainerPhase::Stopping
            | ContainerPhase::Ended => None,
        }
    }

    /// What a status column says in place of `created`.
    pub fn words(self) -> &'static str {
        match self {
            Self::StartUnknown => "start unknown: stop it before retrying",
            Self::PrepareAbandoned => "preparation unfinished: stop it before retrying",
            Self::Unknown => "launch blocked: stop it before retrying",
        }
    }

    /// Why the daemon stopped there and what a person does about it, for
    /// a reader with room for more than the status words.
    pub fn why(self) -> &'static str {
        match self {
            Self::StartUnknown => {
                "Its earlier start's outcome is unknown and the engine still reports it created, so the daemon will not start it again. Stop it, then start a new agent."
            }
            Self::PrepareAbandoned => {
                "Its launch ended before it recorded creating a container, so the daemon will not create one on a guess. Stop it, then start a new agent."
            }
            Self::Unknown => {
                "A newer daemon refused to go on with its launch. Stop it, then start a new agent."
            }
        }
    }
}

impl crate::AgentRecord {
    /// The dead end its container is recorded at, while that still stands:
    /// the agent is live, still meant to run and not seen to start. A stop
    /// or an observed run or exit supersedes it.
    pub fn container_dead_end(&self) -> Option<ContainerDeadEnd> {
        let container = self.container.as_ref()?;
        (self.status == crate::AgentStatus::Created && container.intent == ContainerIntent::Run)
            .then_some(container.dead_end)
            .flatten()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerRunOptions {
    /// Use an engine-volume socket and a CLI stream relay across a VM boundary.
    #[serde(default)]
    pub engine_relay: bool,
    #[serde(default)]
    pub mount_checkout: bool,
    #[serde(default)]
    pub podman_machine: Option<String>,
    #[serde(default)]
    pub network: ContainerNetwork,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerNetwork {
    #[default]
    None,
    Bridge,
}
impl ContainerNetwork {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Bridge => "bridge",
        }
    }
}

/// Persisted mount and UID mapping. Credentials themselves never enter the registry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerWorkspace {
    #[serde(default)]
    pub git: Option<GitMounts>,
    pub checkout: PathBuf,
    pub user: String,
    pub keep_id: bool,
    pub read_only: bool,
    pub access: Option<WorkspaceAccess>,
}

/// Linked checkout metadata mounted separately from the working files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitMounts {
    /// Git directory relative to the common directory, preserving linked-worktree layout.
    pub directory: PathBuf,
    pub common: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceAccess {
    #[serde(default)]
    pub relay: Option<EngineRelay>,
    pub grant: String,
    pub directory: PathBuf,
    pub socket_directory: PathBuf,
    pub vm: Option<PodmanVm>,
}

/// Rebuildable transport resources, separate from the agent writer's identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineRelay {
    pub name: String,
    pub volume: String,
    pub build: ImageBuild,
    #[serde(default)]
    pub retired: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodmanVm {
    pub machine: String,
    pub port: u16,
    pub identity: PathBuf,
    pub user: String,
}

/// Checks are reusable only in this same image and execution configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerEnvironment {
    #[serde(default)]
    pub inputs: Option<ImageInputs>,
    pub image_id: String,
    pub build: String,
    pub engine: ContainerEngine,
    pub connection: Option<String>,
    pub network: ContainerNetwork,
    pub user: Option<String>,
    pub env: std::collections::BTreeMap<String, String>,
}
impl ContainerEnvironment {
    pub fn matches(left: &Option<Self>, right: &Option<Self>) -> bool {
        match (left, right) {
            (None, None) => true,
            (Some(a), Some(b)) if a.inputs.is_some() && b.inputs.is_some() => {
                a.inputs == b.inputs
                    && a.image_id == b.image_id
                    && a.engine == b.engine
                    && a.network == b.network
                    && a.user == b.user
                    && a.env == b.env
            }
            // Older evidence cannot prove portable input identity.
            _ => left == right,
        }
    }

    pub fn of(record: &crate::AgentRecord) -> Option<Self> {
        let c = record.container.as_ref()?;
        Some(Self {
            inputs: c.inputs.clone(),
            image_id: c.image_id.clone(),
            build: c.build.clone(),
            engine: c.engine,
            connection: c.connection.clone(),
            network: c.options.network,
            user: c.workspace.as_ref().map(|w| w.user.clone()),
            env: record.spec.env.clone(),
        })
    }
}

/// Where a managed container's launch stands, read from its agent's
/// status, its write-ahead flags and one fact the record cannot hold.
/// Derived, never stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerPhase {
    /// Being prepared by this process; nothing exists in the engine yet.
    Prepared,
    /// Create was asked for and the engine's id is not known yet. A lost
    /// create response is found again by the container's name.
    Creating,
    /// Created in the engine and never asked to start.
    Created,
    /// Its first start is being issued by this process.
    Starting,
    Running,
    /// A start was asked for, its outcome was never seen, and no process
    /// here is still issuing it. Another start is refused; inspecting it
    /// again is not, and an observed run or exit ends the uncertainty.
    StartUnknown,
    /// Preparation was left unfinished and nothing was created, and no
    /// process here is still preparing it.
    PrepareAbandoned,
    /// Asked to stop or be killed, and not yet seen to exit.
    Stopping,
    /// Its agent is no longer live.
    Ended,
}

impl ContainerPhase {
    /// The phase of a container whose agent has `status`.
    ///
    /// `operating` is positive evidence that this process owns an active
    /// preparation or first start of this container, whichever request
    /// issues it: from the record's publication through create, and the
    /// first start, whether the original launch issues it or a recovery
    /// does after a lost create response. It is cleared when that operation
    /// completes or is cancelled. Neither the original launch request alone
    /// nor the agent being busy is such evidence: a recovery that only
    /// inspects an uncertain start is busy, with nothing to complete.
    ///
    /// Precedence, over every combination of the inputs: a status that is
    /// not live is ended; a stop or kill intent is stopping; a run intent
    /// with status stopping is stopping; with status running, running,
    /// whatever the flags say, since that is the engine's word; with status
    /// created, in order, no create attempted is prepared (operating) or
    /// prepare abandoned, no engine id is creating, no start attempted is
    /// created, and a start attempted is starting (operating) or start
    /// unknown. The pairs the daemon never writes are read conservatively,
    /// never as engine evidence: a run intent with status stopping as a
    /// stop under way, a start flag without a create attempt as unprepared,
    /// and a start flag without an engine id as creating.
    pub fn of(status: &crate::AgentStatus, container: &ManagedContainer, operating: bool) -> Self {
        use crate::AgentStatus;
        match status {
            AgentStatus::Exited { .. } | AgentStatus::Failed { .. } => Self::Ended,
            _ if container.intent != ContainerIntent::Run => Self::Stopping,
            AgentStatus::Stopping => Self::Stopping,
            AgentStatus::Running => Self::Running,
            AgentStatus::Created if !container.create_attempted => {
                if operating {
                    Self::Prepared
                } else {
                    Self::PrepareAbandoned
                }
            }
            AgentStatus::Created if container.id.is_none() => Self::Creating,
            AgentStatus::Created if !container.start_attempted => Self::Created,
            AgentStatus::Created if operating => Self::Starting,
            AgentStatus::Created => Self::StartUnknown,
        }
    }

    /// For the two dead ends, what to do: stop the container before
    /// retrying. Nothing further is started or prepared for it, though it
    /// may still be inspected.
    pub fn dead_end(self) -> Option<&'static str> {
        match self {
            Self::StartUnknown => Some(
                "previous start outcome is unknown; stop this container before retrying as a new agent",
            ),
            Self::PrepareAbandoned => {
                Some("launch preparation did not finish; stop before starting a replacement")
            }
            Self::Prepared
            | Self::Creating
            | Self::Created
            | Self::Starting
            | Self::Running
            | Self::Stopping
            | Self::Ended => None,
        }
    }
}

fn legacy_create_attempted() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentStatus;

    fn container() -> ManagedContainer {
        ManagedContainer {
            inputs: None,
            build: "build".into(),
            engine: ContainerEngine::Podman,
            connection: None,
            image_id: "sha256:image".into(),
            name: "agent-container".into(),
            owner: "owner-label".into(),
            id: None,
            intent: ContainerIntent::Run,
            start_attempted: false,
            create_attempted: false,
            last_error: None,
            options: ContainerRunOptions::default(),
            workspace: None,
            deadline: None,
            dead_end: None,
        }
    }

    fn phase(
        status: &AgentStatus,
        intent: ContainerIntent,
        (create, id, start): (bool, bool, bool),
        operating: bool,
    ) -> ContainerPhase {
        let mut c = container();
        c.intent = intent;
        c.create_attempted = create;
        c.id = id.then(|| "engine-id".to_owned());
        c.start_attempted = start;
        ContainerPhase::of(status, &c, operating)
    }

    /// One row per phase, both sides of `operating` where it decides, and
    /// the pairs the daemon never writes.
    #[test]
    fn each_launch_phase_is_read_from_the_record_and_whether_this_process_operates_it() {
        use ContainerIntent::{Kill, Run, Stop};
        use ContainerPhase::*;
        let created = AgentStatus::Created;
        let rows = [
            // Preparation, by this process or left behind.
            (&created, Run, (false, false, false), true, Prepared),
            (
                &created,
                Run,
                (false, false, false),
                false,
                PrepareAbandoned,
            ),
            (&created, Run, (true, false, false), false, Creating),
            (&created, Run, (true, false, false), true, Creating),
            (&created, Run, (true, true, false), false, Created),
            // The first start, being issued, or its outcome lost.
            (&created, Run, (true, true, true), true, Starting),
            (&created, Run, (true, true, true), false, StartUnknown),
            (
                &AgentStatus::Running,
                Run,
                (true, true, true),
                false,
                Running,
            ),
            (
                &AgentStatus::Stopping,
                Stop,
                (true, true, true),
                false,
                Stopping,
            ),
            (&created, Kill, (true, true, false), false, Stopping),
            (&created, Stop, (false, false, false), false, Stopping),
            (
                &AgentStatus::Exited { code: Some(0) },
                Run,
                (true, true, true),
                false,
                Ended,
            ),
            (
                &AgentStatus::Failed { reason: "x".into() },
                Run,
                (false, false, false),
                true,
                Ended,
            ),
            // Never written, read conservatively.
            (
                &AgentStatus::Stopping,
                Run,
                (true, true, true),
                false,
                Stopping,
            ),
            (
                &AgentStatus::Running,
                Run,
                (false, false, false),
                false,
                Running,
            ),
            (&created, Run, (false, false, true), false, PrepareAbandoned),
            (&created, Run, (false, false, true), true, Prepared),
            (&created, Run, (true, false, true), false, Creating),
        ];
        for (status, intent, flags, operating, expected) in rows {
            assert_eq!(
                phase(status, intent, flags, operating),
                expected,
                "{status:?} {intent:?} {flags:?} operating={operating}"
            );
        }
    }

    /// Every combination of the inputs keeps the precedence, and only the
    /// two dead ends are dead ends, never while this process operates.
    #[test]
    fn the_precedence_holds_over_every_combination() {
        let statuses = [
            AgentStatus::Created,
            AgentStatus::Running,
            AgentStatus::Stopping,
            AgentStatus::Exited { code: None },
            AgentStatus::Failed {
                reason: "fixture".into(),
            },
        ];
        let intents = [
            ContainerIntent::Run,
            ContainerIntent::Stop,
            ContainerIntent::Kill,
        ];
        let mut seen = 0;
        for status in &statuses {
            for intent in intents {
                for bits in 0..8u8 {
                    let flags = (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
                    let with = phase(status, intent, flags, true);
                    let without = phase(status, intent, flags, false);
                    // Both sides of `operating`, in one step.
                    seen += 2;
                    let case = format!("{status:?} {intent:?} {flags:?}");
                    if !status.is_live() {
                        assert_eq!(
                            (with, without),
                            (ContainerPhase::Ended, ContainerPhase::Ended),
                            "{case}"
                        );
                    } else if intent != ContainerIntent::Run || *status == AgentStatus::Stopping {
                        assert_eq!(
                            (with, without),
                            (ContainerPhase::Stopping, ContainerPhase::Stopping),
                            "{case}"
                        );
                    } else if *status == AgentStatus::Running {
                        assert_eq!(
                            (with, without),
                            (ContainerPhase::Running, ContainerPhase::Running),
                            "{case}"
                        );
                    }
                    // Operating decides only between a step in flight and
                    // the same step found left behind.
                    match without {
                        ContainerPhase::PrepareAbandoned => {
                            assert_eq!(with, ContainerPhase::Prepared, "{case}")
                        }
                        ContainerPhase::StartUnknown => {
                            assert_eq!(with, ContainerPhase::Starting, "{case}")
                        }
                        other => assert_eq!(with, other, "{case}"),
                    }
                    assert_eq!(with.dead_end(), None, "{case}");
                    assert_eq!(
                        without.dead_end().is_some(),
                        matches!(
                            without,
                            ContainerPhase::StartUnknown | ContainerPhase::PrepareAbandoned
                        ),
                        "{case}"
                    );
                }
            }
        }
        assert_eq!(seen, 240);
        assert!(
            ContainerPhase::StartUnknown
                .dead_end()
                .unwrap()
                .contains("stop this container before retrying")
        );
        assert!(
            ContainerPhase::PrepareAbandoned
                .dead_end()
                .unwrap()
                .contains("stop before starting a replacement")
        );
    }

    /// The dead end is kept in a form older readers skip: absent when there
    /// is none, none from a record written without it, and a word from a
    /// newer daemon read as `Unknown` rather than refused.
    #[test]
    fn a_dead_end_is_stored_beside_the_flags_and_read_tolerantly() {
        let mut c = container();
        let without = serde_json::to_value(&c).unwrap();
        assert!(without.get("dead_end").is_none(), "{without}");
        let read: ManagedContainer = serde_json::from_value(without.clone()).unwrap();
        assert_eq!(read.dead_end, None);
        for (dead_end, word) in [
            (ContainerDeadEnd::StartUnknown, "start_unknown"),
            (ContainerDeadEnd::PrepareAbandoned, "prepare_abandoned"),
        ] {
            c.dead_end = Some(dead_end);
            let json = serde_json::to_value(&c).unwrap();
            assert_eq!(json["dead_end"], word);
            let read: ManagedContainer = serde_json::from_value(json).unwrap();
            assert_eq!(read, c);
        }
        let mut newer = without;
        newer["dead_end"] = "a_newer_dead_end".into();
        let read: ManagedContainer = serde_json::from_value(newer).unwrap();
        assert_eq!(read.dead_end, Some(ContainerDeadEnd::Unknown));
        // Only the two dead-end phases are dead ends, and each one says
        // what a person does about it.
        use ContainerPhase::*;
        for phase in [
            Prepared,
            Creating,
            Created,
            Starting,
            Running,
            StartUnknown,
            PrepareAbandoned,
            Stopping,
            Ended,
        ] {
            assert_eq!(
                ContainerDeadEnd::of(phase).is_some(),
                phase.dead_end().is_some(),
                "{phase:?}"
            );
        }
        for dead_end in [
            ContainerDeadEnd::StartUnknown,
            ContainerDeadEnd::PrepareAbandoned,
            ContainerDeadEnd::Unknown,
        ] {
            assert!(dead_end.words().ends_with(": stop it before retrying"));
            assert!(dead_end.why().ends_with("Stop it, then start a new agent."));
        }
    }

    /// A record shows its dead end only while it stands: live, still meant
    /// to run and not seen to start.
    #[test]
    fn a_dead_end_is_read_only_while_it_stands() {
        let mut record =
            crate::AgentRecord::new(crate::AgentSpec::default(), true, chrono::Utc::now());
        assert_eq!(record.container_dead_end(), None);
        let mut c = container();
        (c.create_attempted, c.start_attempted) = (true, true);
        c.id = Some("engine-id".into());
        c.dead_end = Some(ContainerDeadEnd::StartUnknown);
        record.container = Some(c);
        assert_eq!(
            record.container_dead_end(),
            Some(ContainerDeadEnd::StartUnknown)
        );
        for status in [
            AgentStatus::Running,
            AgentStatus::Stopping,
            AgentStatus::Exited { code: None },
            AgentStatus::Failed {
                reason: "fixture".into(),
            },
        ] {
            let mut other = record.clone();
            other.status = status;
            assert_eq!(other.container_dead_end(), None, "{:?}", other.status);
        }
        for intent in [ContainerIntent::Stop, ContainerIntent::Kill] {
            let mut other = record.clone();
            other.container.as_mut().unwrap().intent = intent;
            assert_eq!(other.container_dead_end(), None, "{intent:?}");
        }
        record.container.as_mut().unwrap().dead_end = None;
        assert_eq!(record.container_dead_end(), None);
    }

    #[test]
    fn portable_environment_ignores_only_local_build_and_connection_names() {
        let environment = ContainerEnvironment {
            inputs: Some(ImageInputs {
                context_version: "inputs".into(),
                recipe_version: "recipe".into(),
                os: "linux".into(),
                architecture: "arm64".into(),
                variant: None,
            }),
            image_id: "sha256:image".into(),
            build: "local-id".into(),
            engine: ContainerEngine::Podman,
            connection: Some("laptop".into()),
            network: ContainerNetwork::None,
            user: Some("1000:1000".into()),
            env: Default::default(),
        };
        let mut remote = environment.clone();
        remote.build = "other-id".into();
        remote.connection = Some("remote-host-local-engine".into());
        let matches =
            |candidate| ContainerEnvironment::matches(&Some(environment.clone()), &Some(candidate));
        assert!(matches(remote.clone()));
        for change in 0..8 {
            let mut changed = remote.clone();
            match change {
                0 => changed.image_id.push('x'),
                1 => changed.inputs.as_mut().unwrap().context_version.push('x'),
                2 => changed.inputs.as_mut().unwrap().recipe_version.push('x'),
                3 => changed.inputs.as_mut().unwrap().architecture.push('x'),
                4 => changed.network = ContainerNetwork::Bridge,
                5 => changed.user = None,
                6 => {
                    changed.env.insert("MODE".into(), "changed".into());
                }
                _ => changed.engine = ContainerEngine::Docker,
            }
            assert!(!matches(changed), "changed execution input {change}");
        }
        remote.inputs = None;
        assert!(!matches(remote.clone()));
        assert!(ContainerEnvironment::matches(
            &Some(remote.clone()),
            &Some(remote.clone())
        ));
        let mut legacy = remote.clone();
        legacy.build.push('x');
        assert!(!ContainerEnvironment::matches(&Some(remote), &Some(legacy)));
        assert!(!ContainerEnvironment::matches(&Some(environment), &None));
        assert!(ContainerEnvironment::matches(&None, &None));
    }
}
