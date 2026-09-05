//! Persisted observations belong to physical paths, independently of logical projects.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A content version observed before an agent reads a file or searches a directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadMark {
    pub path: PathBuf,
    pub at: DateTime<Utc>,
    pub version: String,
    pub head: Option<String>,
}

/// A retained observation whose content no longer matches the physical checkout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StalePath {
    pub path: PathBuf,
    pub observed: String,
    pub current: Option<String>,
    pub reason: String,
}
