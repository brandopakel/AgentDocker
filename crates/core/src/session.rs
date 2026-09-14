//! The wire between the daemon and a session owner: the small helper
//! process that holds a managed agent's child, terminal, pipes and log so
//! the daemon can be replaced, or die, without the agent noticing.
//!
//! One owner per managed agent. The daemon is its controller: it tells the
//! owner when the durable launch record is committed (`Activate`), what to
//! type, how big the window is, and when to stop. The owner reports the
//! prepared process identity, output as it arrives, and the exit status
//! exactly once. Nothing here does I/O; the framing is newline-delimited
//! JSON, one message per line, like the host protocol.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::AgentId;

/// Bumped only when a stored or transmitted meaning changes.
pub const FORMAT: u32 = 1;

/// How long an owner waits for `Activate` before denying exec and
/// exiting, so an abandoned launch never runs a command nobody recorded.
pub const ACTIVATE_WITHIN_SECS: u64 = 30;

/// The widest a session socket name gets: a 32-hex agent id and `.sock`.
const WIDEST_SOCKET_NAME: &str = "ffffffffffffffffffffffffffffffff.sock";

/// The directory session sockets live in: beside the daemon's own socket
/// when a full agent id still fits there, else the short private
/// directory the daemon falls back to for long homes. One letter, for the
/// same reason.
pub fn sessions_dir(home: &std::path::Path) -> PathBuf {
    let beside = crate::paths::socket_dir(home).join("s");
    if crate::paths::fits_socket(&beside.join(WIDEST_SOCKET_NAME)) {
        return beside;
    }
    crate::paths::short_socket_dir(home).join("s")
}

/// Where an owner serves its controller.
pub fn socket_path(home: &std::path::Path, agent: &AgentId) -> PathBuf {
    sessions_dir(home).join(format!("{}.sock", agent.as_str()))
}

/// Where an owner leaves its final report until a controller acknowledges
/// it, so an exit during a daemon replacement is never lost.
pub fn exit_path(home: &std::path::Path, agent: &AgentId) -> PathBuf {
    sessions_dir(home).join(format!("{}.exit", agent.as_str()))
}

/// The owner process as the daemon records it on the agent, so a daemon
/// that restarts can tell a live owner from a recycled pid.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOwner {
    pub pid: u32,
    pub started_at: DateTime<Utc>,
}

/// The first line an owner sends to any controller.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerHello {
    pub format: u32,
    pub agent: AgentId,
    /// The owner's own pid and birth, so a controller can tell a fresh
    /// owner from a recycled pid.
    pub owner_pid: u32,
    pub owner_started_at: DateTime<Utc>,
    /// The child, once prepared; absent while the launch is still pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child: Option<ChildIdentity>,
    /// Whether the child has been authorised to exec.
    pub activated: bool,
    /// Bytes of terminal output produced so far; `Attach { after }` resumes
    /// from an offset a previous controller had already relayed.
    pub output_offset: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildIdentity {
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    pub tty: bool,
}

/// What a controller may ask of an owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum OwnerCommand {
    /// The launch record and its event are durable: let the command run.
    Activate,
    /// Signal the child's process group; `force` sends SIGKILL.
    Stop { force: bool },
    /// Keystrokes for the terminal.
    Input { bytes: Vec<u8> },
    /// The terminal's window changed.
    Resize { cols: u16, rows: u16 },
    /// Stream output from this offset onward; earlier bytes were already
    /// relayed by a previous controller.
    Attach { after: u64 },
    /// The exit report was recorded durably; the owner may exit.
    Acknowledge,
}

/// What an owner tells its controller.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum OwnerReport {
    Prepared {
        child: ChildIdentity,
    },
    Activated,
    /// Raw output, with the offset of its first byte.
    Output {
        offset: u64,
        bytes: Vec<u8>,
    },
    /// Bytes in `from..to` were produced while no controller was attached
    /// and have scrolled out of the owner's retention; the log has them,
    /// the live view does not. Never implied, always said.
    Gap {
        from: u64,
        to: u64,
    },
    /// Output can no longer be captured; the child is being stopped.
    OutputFailed {
        reason: String,
    },
    /// A keystroke was refused because the terminal's input queue was
    /// full; the controller may retry, nothing was typed.
    InputDropped,
    /// The child and its process group are gone; sent once, and kept in
    /// the exit file until acknowledged.
    Exited {
        status: ExitReport,
    },
}

/// An exit status that survives serialisation: the code, or the signal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitReport {
    /// Whose exit this is: the agent, the owner that held it and the child
    /// it held, so a report from another generation of the same agent id
    /// is never taken for this one.
    pub agent: AgentId,
    pub owner: SessionOwner,
    pub child: ChildIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    /// The log was flushed before this report was written.
    pub log_flushed: bool,
    pub at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_and_reports_round_trip_with_stable_tags() {
        let command = OwnerCommand::Input {
            bytes: b"ls\n".to_vec(),
        };
        let text = serde_json::to_string(&command).unwrap();
        assert!(text.starts_with(r#"{"op":"input""#), "{text}");
        assert_eq!(
            serde_json::from_str::<OwnerCommand>(&text).unwrap(),
            command
        );
        let report = OwnerReport::Exited {
            status: ExitReport {
                agent: AgentId::from("abc"),
                owner: SessionOwner {
                    pid: 7,
                    started_at: Utc::now(),
                },
                child: ChildIdentity {
                    pid: 8,
                    started_at: Utc::now(),
                    tty: false,
                },
                code: Some(0),
                signal: None,
                log_flushed: true,
                at: Utc::now(),
            },
        };
        let text = serde_json::to_string(&report).unwrap();
        assert!(text.starts_with(r#"{"event":"exited""#), "{text}");
        assert!(
            !text.contains("signal"),
            "absent fields are omitted: {text}"
        );
        assert_eq!(serde_json::from_str::<OwnerReport>(&text).unwrap(), report);
    }

    #[test]
    fn owner_paths_sit_beside_the_daemon_socket_and_carry_the_agent_id() {
        let home = std::path::Path::new("/tmp/h");
        let agent = AgentId::from("abc");
        assert_eq!(
            socket_path(home, &agent),
            PathBuf::from("/tmp/h/s/abc.sock")
        );
        assert_eq!(exit_path(home, &agent), PathBuf::from("/tmp/h/s/abc.exit"));
    }

    #[cfg(unix)]
    #[test]
    fn long_owner_socket_paths_use_the_short_directory() {
        // A home too long for a socket name uses the daemon's short socket
        // directory, so a full agent id still fits.
        let long = PathBuf::from(format!("/tmp/{}", "h".repeat(120)));
        let socket = socket_path(&long, &AgentId::from("a".repeat(32).as_str()));
        assert!(crate::paths::fits_socket(&socket), "{}", socket.display());
        assert!(!socket.starts_with(&long));
    }
}
