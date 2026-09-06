//! Explicit, durable session handoff and validation evidence.
use crate::{AgentId, ReadMark, StalePath};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: String,
    pub from: AgentId,
    pub checkout: PathBuf,
    pub created_at: DateTime<Utc>,
    pub task: String,
    pub assumptions: Vec<String>,
    pub next_steps: Vec<String>,
    pub reads: Vec<ReadMark>,
    /// Complete ignore-aware checkout identity at the journal barrier.
    pub version: String,
    #[serde(default)]
    pub environment: Option<crate::container::ContainerEnvironment>,
    pub accepted_by: Option<AgentId>,
    pub release_leases: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Validation {
    pub id: String,
    pub agent: AgentId,
    pub checkout: PathBuf,
    pub command: Vec<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub head: Option<String>,
    pub before: String,
    pub after: Option<String>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub descendants_survived: bool,
    pub log: PathBuf,
    #[serde(default)]
    pub environment: Option<crate::container::ContainerEnvironment>,
    /// Set only after inspection confirms the validation container has exited.
    #[serde(default)]
    pub container: Option<ValidationContainer>,
    #[serde(default)]
    pub error: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationContainer {
    pub agent: AgentId,
    pub id: String,
}

impl Validation {
    /// Successful exit only counts for unchanged content and completed processes.
    pub fn passed(&self) -> bool {
        self.error.is_none()
            && (self.environment.is_none() || self.container.is_some())
            && self.exit_code == Some(0)
            && !self.timed_out
            && !self.descendants_survived
            && self.after.as_ref() == Some(&self.before)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recovery {
    pub checkpoint: Checkpoint,
    pub stale: Vec<StalePath>,
    pub checkout_matches: bool,
    pub environment_matches: bool,
    pub validations: Vec<Validation>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentRecord, AgentSpec, ContainerEngine,
        container::{ContainerEnvironment, ContainerIntent, ManagedContainer},
    };
    #[test]
    fn image_evidence_requires_confirmed_runner_and_exact_environment() {
        let now = DateTime::parse_from_rfc3339("2026-09-05T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut record = AgentRecord::new(AgentSpec::default(), true, now);
        record.container = Some(ManagedContainer {
            inputs: None,
            build: "build".into(),
            engine: ContainerEngine::Docker,
            connection: Some("local".into()),
            image_id: "sha256:image".into(),
            name: "owned".into(),
            owner: "owner".into(),
            id: None,
            intent: ContainerIntent::Run,
            start_attempted: false,
            create_attempted: false,
            last_error: None,
            options: Default::default(),
            workspace: None,
            deadline: None,
        });
        let environment = ContainerEnvironment::of(&record);
        let mut validation = Validation {
            id: "validation".into(),
            agent: record.id.clone(),
            checkout: "/checkout".into(),
            command: vec!["true".into()],
            started_at: now,
            finished_at: now,
            head: None,
            before: "source".into(),
            after: Some("source".into()),
            exit_code: Some(0),
            timed_out: false,
            descendants_survived: false,
            log: "/log".into(),
            environment: environment.clone(),
            container: None,
            error: None,
        };
        assert!(
            !validation.passed(),
            "an engine client exit alone never passes"
        );
        validation.container = Some(ValidationContainer {
            agent: record.id.clone(),
            id: "container".into(),
        });
        assert!(validation.passed());
        record.container.as_mut().unwrap().image_id = "sha256:changed".into();
        assert_ne!(ContainerEnvironment::of(&record), environment);
        validation.after = Some("changed".into());
        assert!(!validation.passed());
        validation.after = Some("source".into());
        validation.timed_out = true;
        assert!(!validation.passed());
        validation.timed_out = false;
        validation.descendants_survived = true;
        assert!(!validation.passed());
    }
}
