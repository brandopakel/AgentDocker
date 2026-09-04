//! `agentdocker up` / `down`: start and stop the agents in an Agentfile.

use std::path::Path;

use agentdocker_core::{AgentRecord, Request, Response};
use anyhow::{Result, bail};

use crate::agentfile::Agentfile;
use crate::client::Client;

/// Start every agent in the file (or just `only`) that is not already live.
/// Prints the id of each agent it started on stdout, one per line, so scripts
/// can capture them; progress goes to stderr.
pub async fn up(client: &Client, file: Option<&Path>, only: &[String]) -> Result<()> {
    let (agentfile, path) = Agentfile::load(file)?;
    let specs = agentfile.specs(&path, only)?;
    let live = live_agents(client).await?;
    let mut failed = 0;
    for spec in specs {
        let name = spec.name.clone();
        if let Some(existing) = live.iter().find(|a| a.spec.name == name) {
            eprintln!("{name:<24} already running   {}", existing.id.short());
            continue;
        }
        match client.call(&Request::Run { spec }).await {
            Ok(Response::Agent { agent }) => {
                eprintln!("{name:<24} started           {}", agent.id.short());
                println!("{}", agent.id);
            }
            Ok(other) => {
                eprintln!("{name:<24} unexpected reply: {other:?}");
                failed += 1;
            }
            Err(err) => {
                eprintln!("{name:<24} failed: {err:#}");
                failed += 1;
            }
        }
    }
    if failed > 0 {
        bail!("{failed} agent(s) failed to start");
    }
    Ok(())
}

/// Stop every live agent named in the file (or just `only`).
pub async fn down(
    client: &Client,
    file: Option<&Path>,
    only: &[String],
    force: bool,
) -> Result<()> {
    let (agentfile, path) = Agentfile::load(file)?;
    let specs = agentfile.specs(&path, only)?;
    let live = live_agents(client).await?;
    let mut failed = 0;
    for spec in specs {
        let name = spec.name;
        let Some(agent) = live.iter().find(|a| a.spec.name == name) else {
            println!("{name:<24} not running");
            continue;
        };
        let request = Request::Stop {
            agent: agent.id.to_string(),
            force,
        };
        match client.call(&request).await {
            Ok(_) => println!("{name:<24} stopping          {}", agent.id.short()),
            Err(err) => {
                eprintln!("{name:<24} failed: {err:#}");
                failed += 1;
            }
        }
    }
    if failed > 0 {
        bail!("{failed} agent(s) failed to stop");
    }
    Ok(())
}

async fn live_agents(client: &Client) -> Result<Vec<AgentRecord>> {
    match client.call(&Request::List { all: false }).await? {
        Response::Agents { agents } => Ok(agents),
        other => bail!("unexpected reply to list: {other:?}"),
    }
}
