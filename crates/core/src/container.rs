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
