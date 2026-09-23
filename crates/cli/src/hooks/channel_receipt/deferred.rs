//! A Stop hook can finish before the provider flushes its final response.
//! Check again after returning control, without manufacturing a provider turn.
use super::super::{HookInput, session_agent};
use crate::client::Client;
use agentdocker_core::{AgentRecord, Request, Response};
use agentdocker_host::{dirs, lock, procinfo};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::Args;
use sha2::{Digest, Sha256};
use std::{path::PathBuf, process::Stdio, time::Duration};

#[derive(Args, Debug)]
pub struct DeferredArgs {
    #[arg(long)]
    agent: String,
    #[arg(long)]
    pid: u32,
    #[arg(long)]
    started_at: DateTime<Utc>,
    #[arg(long)]
    session: String,
    #[arg(long)]
    transcript: PathBuf,
}

impl DeferredArgs {
    fn matches(&self, agent: &AgentRecord) -> bool {
        agent.id.as_str() == self.agent
            && agent.pid == Some(self.pid)
            && agent.process_started_at == Some(self.started_at)
            && agent.spec.runtime == "claude-code"
            && agent.spec.labels.get("session_id") == Some(&self.session)
            && agent.status.is_live()
            && procinfo::start_time(self.pid) == Some(self.started_at)
    }
}

pub(crate) async fn schedule(client: &Client, input: &HookInput) -> Result<()> {
    if input.agent_id.is_some() {
        return Ok(());
    }
    let Some(transcript) = input.transcript_path.as_deref() else {
        return Ok(());
    };
    let Some(agent) = session_agent(client, input).await? else {
        return Ok(());
    };
    let (Some(pid), Some(started)) = (agent.pid, agent.process_started_at) else {
        return Ok(());
    };
    if agent.spec.labels.get("session_id") != Some(&input.session_id)
        || !crate::mcp::channel_input_active(&dirs::home(), &agent)?
    {
        return Ok(());
    }
    // This command gets no provider stdin/stdout and cannot start a daemon.
    // It lives for at most three seconds and rechecks the exact generation.
    let mut child = std::process::Command::new(procinfo::executable_path()?)
        .arg("--socket")
        .arg(client.socket_path())
        .args(["hook", "claude-receipt", "--agent"])
        .arg(agent.id.as_str())
        .arg("--pid")
        .arg(pid.to_string())
        .arg("--started-at")
        .arg(started.to_rfc3339())
        .arg("--session")
        .arg(&input.session_id)
        .arg("--transcript")
        .arg(transcript)
        .env("AGENTDOCKER_NO_AUTOSTART", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("start deferred channel receipt check")?;
    // Usually the hook exits first and the OS reaps this short-lived child.
    // Reap immediately if it has already refused its arguments/identity.
    let _ = child.try_wait();
    Ok(())
}

pub async fn run(client: Client, args: DeferredArgs) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let _ = tokio::time::timeout_at(deadline, async {
        let home = dirs::home();
        let directory = home.join("channel-receipts");
        dirs::ensure_private_dir(&directory)?;
        let key = format!("worker-{:x}.lock", Sha256::digest(args.agent.as_bytes()));
        let path = directory.join(key);
        dirs::private_file(&path, true, false)?;
        let _guard = loop {
            if let Some(guard) = lock::try_exclusive_existing(&path)? {
                break guard;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let input = HookInput {
            session_id: args.session.clone(),
            transcript_path: Some(args.transcript.clone()),
            ..HookInput::default()
        };
        for delay in [150, 300, 600] {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            let Response::Agent { agent } = client
                .call(&Request::Inspect {
                    agent: args.agent.clone(),
                })
                .await?
            else {
                break;
            };
            if !args.matches(&agent) || !crate::mcp::channel_input_active(&home, &agent)? {
                break;
            }
            // One bounded proof attempt; a transient deadline can retry, but
            // there is never a synthetic lifecycle/contact/activity report.
            if tokio::time::timeout(
                Duration::from_millis(350),
                super::recover(&client, &input, &agent, &home),
            )
            .await
            .is_ok_and(|result| result.unwrap_or(false))
            {
                break;
            }
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;
    Ok(())
}
