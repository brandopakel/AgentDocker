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
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerRunOptions {
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
    pub checkout: PathBuf,
    pub user: String,
    pub keep_id: bool,
    pub read_only: bool,
    pub access: Option<WorkspaceAccess>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceAccess {
    pub grant: String,
    pub directory: PathBuf,
    pub socket_directory: PathBuf,
    pub vm: Option<PodmanVm>,
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
    pub image_id: String,
    pub build: String,
    pub engine: ContainerEngine,
    pub connection: Option<String>,
    pub network: ContainerNetwork,
    pub user: Option<String>,
    pub env: std::collections::BTreeMap<String, String>,
}
impl ContainerEnvironment {
    pub fn of(record: &crate::AgentRecord) -> Option<Self> {
        let c = record.container.as_ref()?;
        Some(Self {
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

fn legacy_create_attempted() -> bool {
    true
}
