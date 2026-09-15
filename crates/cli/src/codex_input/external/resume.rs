//! Rejoin a restarted TUI to its canonical queue without starting its thread.
use super::{alive, bootstrap, call, ledger::Binding};
use crate::client::Client;
use agentdocker_core::{AgentRecord, ControllerLaunch, Request, Response};
use agentdocker_host::procinfo;
use anyhow::{Context, Result, bail, ensure};
use std::time::Duration;

/// Read-only discovery. The daemon rechecks these conditions atomically when
/// the detached receiver requests the handoff, after the old receiver exits.
pub(super) async fn predecessor(client: &Client, binding: &Binding) -> Result<Option<String>> {
    let Response::Agents { agents, .. } = call(
        client,
        Request::List {
            all: true,
            project: None,
            labels: Default::default(),
        },
    )
    .await?
    else {
        bail!("native input could not inspect prior conversation bindings");
    };
    let mut matching = agents.iter().filter(|agent| {
        agent.id.as_str() != binding.agent
            && agent.input_binding.as_ref().is_some_and(|bound| {
                bound.provider.session == binding.provider.session
                    && bound.provider.profile == binding.provider.profile
            })
    });
    let Some(prior) = matching.next() else {
        return Ok(None);
    };
    ensure!(
        matching.next().is_none(),
        "more than one record is bound to this Codex conversation"
    );
    verify_prior(prior, binding)?;
    Ok(Some(prior.id.to_string()))
}

fn verify_prior(prior: &AgentRecord, binding: &Binding) -> Result<()> {
    let old = prior
        .input_binding
        .as_ref()
        .context("prior conversation binding is unavailable")?;
    ensure!(
        prior.spec.runtime == "codex"
            && !prior.managed
            && old.provider.session == binding.provider.session
            && old.provider.profile == binding.provider.profile
            && prior
                .spec
                .workdir
                .as_ref()
                .and_then(|p| p.canonicalize().ok())
                .as_ref()
                == Some(&binding.cwd),
        "prior native input belongs to a different conversation or checkout"
    );
    ensure!(
        procinfo::start_time(old.provider.process.pid) != Some(old.provider.process.started_at),
        "another live Codex process owns this conversation's input"
    );
    Ok(())
}

fn accepted(
    response: &Response,
    binding: &Binding,
    retired: &str,
    launch: &ControllerLaunch,
) -> bool {
    matches!(response, Response::InputResumed { agent, retired: observed, binding: bound }
        if agent.as_str() == binding.agent && observed.as_str() == retired
            && bound.provider == binding.provider && bound.launch.as_ref() == Some(launch))
}

pub(super) async fn handoff(
    client: &Client,
    mut binding: Binding,
    predecessor: &str,
) -> Result<Binding> {
    let retired = binding.agent.clone();
    ensure!(
        predecessor != retired,
        "native input cannot resume into itself"
    );
    binding.agent = predecessor.into();
    let launch = bootstrap::launch(&binding)?;
    // The hook only starts this detached helper. Waiting here lets the old
    // receiver release its lock without delaying the person's submitted input.
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            ensure!(alive(&binding), "resumed Codex exited before queue handoff");
            let Response::Agent { agent: prior } = call(
                client,
                Request::Inspect {
                    agent: predecessor.into(),
                },
            )
            .await?
            else {
                bail!("prior Codex input record is unavailable");
            };
            if prior
                .input_binding
                .as_ref()
                .is_some_and(|b| b.provider == binding.provider)
            {
                // A lost successful response must still prove this exact
                // retired alias, generation and immutable launch descriptor.
                let response = call(
                    client,
                    Request::ResumeInput {
                        agent: retired.clone(),
                        predecessor: predecessor.into(),
                        provider: binding.provider.clone(),
                        launch: launch.clone(),
                    },
                )
                .await;
                let Ok(response) = response else {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                };
                ensure!(
                    accepted(&response, &binding, &retired, &launch),
                    "native input resume receipt does not match this handoff"
                );
                return Ok(binding);
            }
            verify_prior(&prior, &binding)?;
            let old = prior.input_binding.as_ref().expect("verified binding");
            let running = |p: &agentdocker_core::ProcessIdentity| {
                procinfo::start_time(p.pid) == Some(p.started_at)
            };
            if running(&old.controller) || old.restart.launched.as_ref().is_some_and(running) {
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            }
            let response = call(
                client,
                Request::ResumeInput {
                    agent: retired.clone(),
                    predecessor: predecessor.into(),
                    provider: binding.provider.clone(),
                    launch: launch.clone(),
                },
            )
            .await;
            let Ok(response) = response else {
                // A lost mutation reply is reconciled through the exact
                // retired alias on the next iteration, never a fresh identity.
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            };
            ensure!(
                accepted(&response, &binding, &retired, &launch),
                "daemon refused native conversation queue handoff"
            );
            return Ok(binding);
        }
    })
    .await
    .context("prior receiver did not release the conversation within thirty seconds")?
}
