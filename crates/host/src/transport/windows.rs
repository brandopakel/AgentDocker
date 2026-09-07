//! Container workspace transport needs a named-pipe-to-VM adapter on Windows.
//! Refuse it before granting credentials or mounting host state. Native process
//! execution does not use this optional engine boundary.
use crate::containers::ContainerError;
use agentdocker_core::{AgentRecord, ErrorCode, container::WorkspaceAccess};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct TransportError {
    pub code: ErrorCode,
    pub message: String,
}
impl From<TransportError> for ContainerError {
    fn from(error: TransportError) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }
}
pub fn storage_error(error: impl std::fmt::Display) -> TransportError {
    TransportError {
        code: ErrorCode::StorageUnavailable,
        message: error.to_string(),
    }
}
fn unsupported() -> TransportError {
    TransportError {
        code: ErrorCode::EngineUnavailable,
        message: "Container workspace transport on Windows is not implemented".into(),
    }
}
pub fn private_directory(path: &Path) -> Result<(), TransportError> {
    crate::dirs::secure_state_dir(path).map_err(storage_error)
}
#[derive(Default)]
pub struct Preparation;
impl Preparation {
    pub fn commit(self) {}
}
pub fn prepare(
    _: &mut AgentRecord,
    _: &Path,
    _: &Path,
    _: &mut Preparation,
) -> Result<(), TransportError> {
    Err(unsupported())
}
pub enum Bridge {}
impl Bridge {
    pub fn alive(&mut self) -> bool {
        match *self {}
    }
}
pub fn bridge(_: &WorkspaceAccess, _: &Path) -> Result<Option<Bridge>, TransportError> {
    Err(unsupported())
}
