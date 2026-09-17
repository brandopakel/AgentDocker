//! Explicit receiver replacement, preserving the existing provider and ledger.
use super::{bootstrap, identity, ledger, preflight};
use crate::client::Client;
use agentdocker_core::{InputReadiness, Request, Response};
use agentdocker_host::{dirs, procinfo};
use anyhow::{Context, Result, bail, ensure};
use chrono::Utc;
use std::time::Duration;

#[derive(clap::Args)]
pub struct Args {
    /// Existing external Codex agent whose receiver should use this CLI release.
    #[arg(long)]
    pub agent: String,
}

pub async fn run(client: Client, args: Args) -> Result<()> {
    let client = client.with_start_timeout(None);
    let Response::Agent { agent } = client.call(&Request::Inspect { agent: args.agent }).await?
    else {
        bail!("the Codex agent is unavailable");
    };
    let accepted = agent
        .input_binding
        .as_ref()
        .context("the session has no bound receiver")?;
    let previous = accepted
        .launch
        .clone()
        .context("the receiver has no recovery descriptor")?;
    let (binding, token) = ledger::upgrade_credential(&dirs::home(), agent.id.as_str(), accepted)?;
    ensure!(
        binding.socket == client.socket_path(),
        "the retained receiver belongs to another daemon endpoint"
    );
    identity(&client, &binding).await?;
    ensure!(
        procinfo::executable_path_of(binding.provider.process.pid)?.canonicalize()?
            == binding.executable,
        "the Codex provider executable changed"
    );
    let launch = bootstrap::launch(&binding)?;
    ensure!(
        launch.args == previous.args && launch.cwd == previous.cwd && launch.env == previous.env,
        "receiver launch arguments changed; automatic replacement is unsafe"
    );
    if launch == previous {
        println!("The receiver already uses this release; no process changed.");
        return Ok(());
    }
    // These are read-only provider queries. No queue entries, private ledger,
    // provider configuration or process is changed during this preflight.
    preflight(&binding).await?;
    let response = client
        .call(&Request::UpgradeController {
            agent: agent.id.to_string(),
            provider: binding.provider.clone(),
            controller: accepted.controller.clone(),
            previous,
            launch: launch.clone(),
            token,
        })
        .await?;
    let Response::InputBound {
        agent: committed_agent,
        binding: committed,
        ..
    } = response
    else {
        bail!("the daemon did not accept the receiver upgrade");
    };
    ensure!(
        committed_agent == agent.id
            && committed.provider == binding.provider
            && committed.launch.as_ref() == Some(&launch),
        "the daemon accepted a different receiver descriptor"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let current = identity(&client, &binding).await?;
        let bound = current
            .input_binding
            .as_ref()
            .context("the receiver binding disappeared during upgrade")?;
        ensure!(
            bound.launch.as_ref() == Some(&launch),
            "another operation changed the receiver descriptor"
        );
        if bound.controller != accepted.controller
            && procinfo::start_time(bound.controller.pid) == Some(bound.controller.started_at)
            && current
                .input_delivery
                .as_ref()
                .is_some_and(|delivery| delivery.reported_at >= bound.controller_since)
            && matches!(
                InputReadiness::for_agent(&current, Utc::now()),
                InputReadiness::Verified | InputReadiness::AwaitingFirstReceipt
            )
        {
            println!(
                "Receiver upgraded to {} (pid {}). Provider pid {}, thread and queued input preserved.",
                launch.executable.display(),
                bound.controller.pid,
                binding.provider.process.pid
            );
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "receiver upgrade is committed, but the successor has not reported ready; the binding and queued input are retained"
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
