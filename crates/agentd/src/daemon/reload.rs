//! Planned daemon replacement is unavailable until ownership transfer is safe.
//!
//! The previous descriptor-only protocol returned success before the successor
//! was ready and dropped live Child handles during runtime shutdown. Actual
//! batch and PTY fixtures were killed. Keep requests side-effect-free while a
//! replacement protocol is implemented and verified across the entire process.

use super::*;

impl Daemon {
    pub(super) async fn hand_over(self: &Arc<Self>) -> Response {
        Response::error(
            ErrorCode::Unavailable,
            "live daemon reload is unavailable: process, I/O and successor readiness transfer are not yet safe; the current daemon and agents remain running",
        )
    }
}
