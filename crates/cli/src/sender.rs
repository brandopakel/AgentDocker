//! A provider's shell commands must not silently speak as the human. Explicit
//! sender flags/environment remain authoritative; ordinary terminals keep the
//! human default. An inferred provider requires one exact live registry owner.
use crate::client::Client;
use agentdocker_core::{AgentRecord, Request, Response};
use agentdocker_host::procinfo::{self, Process};
use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, BTreeSet};

pub async fn resolve(client: &Client, explicit: Option<String>) -> Result<Option<String>> {
    if explicit.is_some() {
        return Ok(explicit);
    }
    let table =
        procinfo::processes().context("cannot determine the CLI sender's process ancestry")?;
    let parent = table
        .iter()
        .find(|process| process.pid == std::process::id())
        .context("CLI caller is missing from the process snapshot; specify the sender explicitly")?
        .ppid;
    let Some((process, runtime)) = provider_ancestor(parent, &table)? else {
        return Ok(None);
    };
    let born = procinfo::start_time(process.pid)
        .context("cannot verify the provider caller's birth time; specify the sender explicitly")?;
    let Response::Agents { agents, .. } = client
        .call(&Request::List {
            all: false,
            project: None,
            labels: BTreeMap::new(),
        })
        .await?
    else {
        bail!("daemon did not identify the provider caller; specify the sender explicitly");
    };
    let sender = select(&agents, process.pid, born, runtime, |agent| {
        agentdocker_host::provider_input::owns_codex_process(agent, process.pid, &table)
    })?;
    ensure!(
        procinfo::start_time(process.pid) == Some(born),
        "provider caller changed while resolving the sender"
    );
    Ok(Some(sender))
}

fn provider_ancestor(mut pid: u32, table: &[Process]) -> Result<Option<(&Process, &'static str)>> {
    let mut visited = BTreeSet::new();
    for _ in 0..64 {
        if pid <= 1 {
            return Ok(None);
        }
        ensure!(
            visited.insert(pid),
            "cyclic CLI process ancestry; specify the sender explicitly"
        );
        let process = table
            .iter()
            .find(|p| p.pid == pid)
            .context("incomplete CLI process ancestry; specify the sender explicitly")?;
        if let Some(runtime) = procinfo::runtime_of(&process.argv) {
            return Ok(Some((process, runtime)));
        }
        pid = process.ppid;
    }
    bail!("CLI process ancestry exceeds its bound; specify the sender explicitly")
}

fn select(
    agents: &[AgentRecord],
    pid: u32,
    born: DateTime<Utc>,
    runtime: &str,
    owns_provider: impl Fn(&AgentRecord) -> bool,
) -> Result<String> {
    let owners: BTreeSet<_> = agents
        .iter()
        .filter(|agent| {
            agent.status.is_live()
                && agent.container.is_none()
                && agent.spec.runtime == runtime
                && ((agent.pid == Some(pid) && agent.process_started_at == Some(born))
                    || owns_provider(agent))
        })
        .map(|agent| agent.id.to_string())
        .collect();
    ensure!(
        owners.len() == 1,
        "provider caller has no unique live registered identity; use its bound MCP tools or specify the sender explicitly"
    );
    Ok(owners.into_iter().next().expect("one owner checked"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{AgentSpec, AgentStatus};

    fn process(pid: u32, ppid: u32, program: &str) -> Process {
        Process {
            pid,
            ppid,
            argv: vec![program.into()],
        }
    }

    #[test]
    fn nested_providers_use_the_nearest_caller_and_unknown_ancestry_never_becomes_human() {
        let table = [
            process(10, 20, "zsh"),
            process(20, 30, "claude"),
            process(30, 1, "codex"),
        ];
        let (caller, runtime) = provider_ancestor(10, &table).unwrap().unwrap();
        assert_eq!((caller.pid, runtime), (20, "claude-code"));
        assert!(
            provider_ancestor(10, &[process(10, 1, "zsh")])
                .unwrap()
                .is_none()
        );
        assert!(provider_ancestor(10, &[process(10, 20, "zsh")]).is_err());
        assert!(provider_ancestor(10, &[process(10, 10, "zsh")]).is_err());
    }

    #[test]
    fn sender_selection_refuses_recycled_missing_or_ambiguous_provider_records() {
        let born = Utc::now();
        let mut agent = AgentRecord::new(
            AgentSpec {
                runtime: "claude-code".into(),
                ..Default::default()
            },
            false,
            born,
        );
        agent.pid = Some(20);
        agent.process_started_at = Some(born);
        agent.status = AgentStatus::Running;
        assert_eq!(
            select(
                std::slice::from_ref(&agent),
                20,
                born,
                "claude-code",
                |_| false
            )
            .unwrap(),
            agent.id.to_string()
        );
        assert!(
            select(
                std::slice::from_ref(&agent),
                20,
                born + chrono::Duration::seconds(1),
                "claude-code",
                |_| false
            )
            .is_err()
        );
        assert!(select(std::slice::from_ref(&agent), 20, born, "codex", |_| false).is_err());
        let mut duplicate = AgentRecord::new(agent.spec.clone(), false, born);
        duplicate.pid = agent.pid;
        duplicate.process_started_at = agent.process_started_at;
        duplicate.status = AgentStatus::Running;
        assert!(
            select(&[agent.clone(), duplicate], 20, born, "claude-code", |_| {
                false
            })
            .is_err()
        );
        agent.status = AgentStatus::Exited { code: Some(0) };
        assert!(select(&[agent], 20, born, "claude-code", |_| false).is_err());
        assert!(select(&[], 20, born, "claude-code", |_| false).is_err());
    }

    #[test]
    fn an_owned_provider_may_use_its_verified_controller_identity() {
        let born = Utc::now();
        let mut owner = AgentRecord::new(
            AgentSpec {
                runtime: "codex".into(),
                ..Default::default()
            },
            true,
            born,
        );
        owner.pid = Some(30);
        owner.process_started_at = Some(born);
        owner.status = AgentStatus::Running;
        assert!(select(std::slice::from_ref(&owner), 40, born, "codex", |_| false).is_err());
        assert_eq!(
            select(std::slice::from_ref(&owner), 40, born, "codex", |_| true).unwrap(),
            owner.id.to_string()
        );
    }
}
