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
}
impl Validation {
    /// Successful exit only counts for unchanged content and completed processes.
    pub fn passed(&self) -> bool {
        self.exit_code == Some(0)
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
    pub validations: Vec<Validation>,
}
