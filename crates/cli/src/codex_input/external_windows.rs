//! The native Codex queue's shape on Windows, where it is not delivered
//! yet: the receiver's hook endpoint is a Unix socket checked by peer
//! credentials, and its owner process a Unix child. Every entry point
//! keeps its signature so the daemon's launch descriptor, the hidden
//! commands and the Codex hook adapter compile unchanged; the hook adapter
//! sees "not started" and takes its ordinary path, so a Codex session on
//! Windows receives its messages at its next prompt, never live.

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::client::Client;

pub const UNAVAILABLE: &str = "the native Codex queue is not available on Windows yet";

#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    pub agent: String,
    #[arg(long)]
    pub pid: u32,
    #[arg(long)]
    pub started_at: DateTime<Utc>,
    #[arg(long)]
    pub thread: String,
    #[arg(long)]
    pub profile: std::path::PathBuf,
    #[arg(long)]
    pub cwd: std::path::PathBuf,
    #[arg(long)]
    pub program: std::path::PathBuf,
}

pub async fn run(_client: Client, _socket: Option<std::path::PathBuf>, _args: Args) -> Result<()> {
    anyhow::bail!(UNAVAILABLE)
}

/// A Codex hook on Windows never has a receiver to feed: the adapter
/// takes its ordinary path.
pub async fn ensure_started(
    _client: &Client,
    _agent: &agentdocker_core::AgentRecord,
) -> Result<bool> {
    Ok(false)
}

pub mod hooks {
    use anyhow::Result;

    pub async fn context(
        _agent: &agentdocker_core::AgentRecord,
        _event: &str,
        _session: &str,
    ) -> Result<Option<String>> {
        Ok(None)
    }
}

pub mod upgrade {
    use anyhow::Result;

    use crate::client::Client;

    #[derive(clap::Args)]
    pub struct Args {
        #[arg(long, env = "AGENTDOCKER_AGENT_ID")]
        pub agent: String,
    }

    pub async fn run(_client: Client, _args: Args) -> Result<()> {
        anyhow::bail!(super::UNAVAILABLE)
    }
}
