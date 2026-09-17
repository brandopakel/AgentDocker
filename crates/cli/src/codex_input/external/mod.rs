//! Feed an existing Codex CLI through its native queue without resuming it.
mod answers;
mod availability;
mod bootstrap;
pub mod hooks;
mod ledger;
mod receipts;
mod resume;
pub use bootstrap::ensure_started;

use super::{call, transport::Provider};
use crate::client::Client;
use agentdocker_core::{
    AgentRecord, Envelope, InputReceipt, InputReport, ProcessIdentity, ProviderGeneration,
    ReceivedInput, Request, Response,
};
use agentdocker_host::{dirs, procinfo};
use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Utc};
use ledger::{Binding, Ledger};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(clap::Args)]
pub struct Args {
    /// Canonical AgentDocker record ID for the verified provider session.
    #[arg(long)]
    pub agent: String,
    /// Existing Codex provider process ID; the receiver never starts that session.
    #[arg(long)]
    pub pid: u32,
    /// Exact provider process birth in RFC3339 format.
    #[arg(long)]
    pub started_at: DateTime<Utc>,
    /// Persisted Codex conversation ID verified by the provider hook.
    #[arg(long)]
    pub thread: String,
    /// Absolute path to the existing provider configuration profile.
    #[arg(long)]
    pub profile: PathBuf,
    /// Absolute physical checkout path for the verified provider session.
    #[arg(long)]
    pub cwd: PathBuf,
    /// Absolute path to the existing provider executable.
    #[arg(long)]
    pub program: PathBuf,
    /// A verified prior binding to this persisted conversation.
    #[arg(long)]
    pub predecessor: Option<String>,
}

fn alive(binding: &Binding) -> bool {
    procinfo::start_time(binding.provider.process.pid) == Some(binding.provider.process.started_at)
}

async fn identity(client: &Client, binding: &Binding) -> Result<AgentRecord> {
    let Response::Agent { agent } = call(
        client,
        Request::Inspect {
            agent: binding.agent.clone(),
        },
    )
    .await?
    else {
        bail!("native queue agent is unavailable");
    };
    ensure!(
        agent.id.as_str() == binding.agent
            && agent.spec.runtime == "codex"
            && !agent.managed
            && agent.status.is_live()
            && agent.pid == Some(binding.provider.process.pid)
            && agent.process_started_at == Some(binding.provider.process.started_at)
            && agent.spec.labels.get("session_id") == Some(&binding.provider.session)
            && agent
                .spec
                .workdir
                .as_ref()
                .and_then(|p| p.canonicalize().ok())
                .as_ref()
                == Some(&binding.cwd)
            && alive(binding),
        "native queue requires the exact registered live Codex conversation"
    );
    Ok(agent)
}

async fn report(client: &Client, ledger: &Ledger, report: InputReport) -> Result<()> {
    let binding = &ledger.record().binding;
    ensure!(
        matches!(
            call(
                client,
                Request::ReportInput {
                    agent: binding.agent.clone(),
                    process_started_at: binding.provider.process.started_at,
                    observed_at: Utc::now(),
                    report,
                    token: Some(ledger.record().token.clone())
                }
            )
            .await?,
            Response::Ok
        ),
        "native input status report refused"
    );
    Ok(())
}

async fn queue(
    client: &Client,
    ledger: &Ledger,
    acknowledge: Vec<agentdocker_core::MessageId>,
) -> Result<(Vec<Envelope>, Vec<agentdocker_core::MessageId>, bool)> {
    match call(
        client,
        Request::ProviderInbox {
            agent: ledger.record().binding.agent.clone(),
            acknowledge,
            token: Some(ledger.record().token.clone()),
        },
    )
    .await?
    {
        Response::InputBatch {
            agent,
            messages,
            uncertain,
            answers_routed,
        } if agent.as_str() == ledger.record().binding.agent => {
            Ok((messages, uncertain, answers_routed))
        }
        Response::InputWaiting { .. } => Ok((Vec::new(), Vec::new(), false)),
        _ => bail!("native input controller no longer owns this queue"),
    }
}

async fn acknowledge(client: &Client, ledger: &mut Ledger) -> Result<()> {
    let attempt = ledger
        .record()
        .attempt
        .as_ref()
        .context("no native input to acknowledge")?;
    let receipt = attempt
        .receipt
        .as_ref()
        .context("native input lacks provider receipt")?;
    let message = attempt.message.clone();
    report(
        client,
        ledger,
        InputReport::Received {
            input: ReceivedInput {
                messages: vec![message.clone().into()],
                receipt: InputReceipt::Codex {
                    thread: receipt.thread.clone(),
                    turn: receipt.turn.clone(),
                    item: receipt.item.clone(),
                },
            },
        },
    )
    .await?;
    queue(client, ledger, vec![message.into()]).await?;
    ledger.acknowledge()
}

async fn verify_provider(provider: &mut Provider, binding: &Binding) -> Result<()> {
    provider.initialize().await?;
    let value = provider
        .request(
            "thread/read",
            json!({"threadId":binding.provider.session,"includeTurns":false}),
        )
        .await?;
    ensure!(
        value["thread"]["id"].as_str() == Some(&binding.provider.session)
            && value["thread"]["cwd"]
                .as_str()
                .and_then(|p| Path::new(p).canonicalize().ok())
                .as_ref()
                == Some(&binding.cwd),
        "provider conversation does not match the verified hook checkout"
    );
    provider
        .request(
            "thread/queue/list",
            json!({"threadId":binding.provider.session,"limit":1}),
        )
        .await
        .context("this Codex version does not expose the native input queue")?;
    // These are read-only calls. In particular, never thread/resume or turn/start:
    // the original TUI owns its lifecycle, draft, tools and permission prompts.
    receipts::latest_item(provider, &binding.provider.session).await?;
    Ok(())
}

async fn preflight(binding: &Binding) -> Result<()> {
    let mut provider = Provider::start_profile(
        &binding.executable,
        &[],
        &binding.cwd,
        Some(Path::new(&binding.provider.profile)),
    )?;
    let checked = verify_provider(&mut provider, binding).await;
    let stopped = provider.shutdown().await;
    checked.and(stopped)
}

async fn service(
    client: &Client,
    provider: &mut Provider,
    ledger: &mut Ledger,
    hooks: &hooks::Listener,
) -> Result<()> {
    let thread = ledger.record().binding.provider.session.clone();
    let human = match call(client, Request::Me { workdir: None }).await? {
        Response::Agent { agent } => agent.id.to_string(),
        _ => bail!("native input human identity is unavailable"),
    };
    let origin = answers::origin(provider, human).await?;
    let mut unseen_since: Option<tokio::time::Instant> = None;
    let mut answer_wait: Option<tokio::time::Instant> = None;
    let mut last_refresh: Option<tokio::time::Instant> = None;
    loop {
        let agent = identity(client, &ledger.record().binding).await?;
        let mut healthy = true;
        if let Some(attempt) = ledger.record().attempt.clone() {
            if attempt.receipt.is_some() {
                acknowledge(client, ledger).await?;
                unseen_since = None;
                continue;
            }
            if let Some(receipt) = receipts::find(provider, &thread, &attempt).await? {
                ledger.received(receipt)?;
                continue;
            }
            if attempt.hook.is_some() {
                healthy = false;
                let since = unseen_since.get_or_insert_with(tokio::time::Instant::now);
                ensure!(
                    since.elapsed() < Duration::from_secs(30),
                    "native hook offer lacks an exact provider receipt; retained without resubmission"
                );
            } else if let Some(id) = receipts::queued(provider, &thread, &attempt).await? {
                if attempt.queued.as_deref() != Some(&id) {
                    ledger.queued(&id)?;
                }
                unseen_since = None;
                // A verified pending entry belongs to the original TUI's
                // scheduler. This read-only sidecar cannot prove live idleness:
                // Codex may reconstruct a still-running turn as "interrupted".
                // Keep reconciling the same entry, without pausing, resubmitting
                // or acknowledging it just because time has elapsed.
            } else {
                healthy = false;
                let since = unseen_since.get_or_insert_with(tokio::time::Instant::now);
                ensure!(
                    since.elapsed() < Duration::from_secs(30),
                    "input has no native queue entry or provider receipt; retained for reconciliation, automatic resubmission refused"
                );
            }
        } else {
            if !availability::check(client, provider, ledger, &agent).await? {
                refresh(client, ledger, &mut last_refresh).await?;
                wait_for_hook(hooks, client, provider, ledger).await?;
                continue;
            }
            let (messages, uncertain, answers_routed) = queue(client, ledger, Vec::new()).await?;
            if let Some(envelope) = messages.first() {
                match answers::route(
                    provider,
                    &origin,
                    &thread,
                    envelope,
                    &ledger.record().binding.agent,
                    answers_routed && !uncertain.contains(&envelope.id),
                )
                .await?
                {
                    answers::Route::Received(receipt) => {
                        answer_wait = None;
                        ledger.prepare(envelope, None)?;
                        ledger.received(receipt)?;
                        continue;
                    }
                    answers::Route::Waiting => {
                        let since = answer_wait.get_or_insert_with(tokio::time::Instant::now);
                        ensure!(
                            since.elapsed() < Duration::from_secs(30),
                            "human answer is waiting for its exact MCP question receipt; it will not be submitted twice"
                        );
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        continue;
                    }
                    answers::Route::Input => {
                        answer_wait = None;
                    }
                }
                ensure!(
                    !uncertain.contains(&envelope.id),
                    "a legacy reader already offered this message; reconcile that receipt before native delivery"
                );
                let anchor = receipts::latest_item(provider, &thread).await?;
                ledger.prepare(envelope, anchor)?;
                let attempt = ledger.record().attempt.as_ref().expect("prepared input");
                let value = provider.request("thread/queue/add", json!({"threadId":thread,
                    "clientUserMessageId":attempt.message,
                    "input":[{"type":"text","text":attempt.input,"text_elements":[]}]})).await
                    .context("native queue submission is unconfirmed; retained for receipt reconciliation")?;
                let id = receipts::queued_id(&value["queuedSubmission"], attempt)?
                    .context("native queue returned a different submission")?;
                ledger.queued(&id)?;
                unseen_since = None;
            }
        }
        // A reconciliation retry must not briefly clear an unresolved pause.
        // Readiness is refreshed only after the outstanding attempt is proven
        // present, or after the next queue head has passed its ownership checks.
        if healthy {
            refresh(client, ledger, &mut last_refresh).await?;
        }
        wait_for_hook(hooks, client, provider, ledger).await?;
    }
}

async fn wait_for_hook(
    hooks: &hooks::Listener,
    client: &Client,
    provider: &mut Provider,
    ledger: &mut Ledger,
) -> Result<()> {
    tokio::select! {
        stream = hooks.accept() => {
            match stream {
                Ok(stream) => if let Err(error) = hooks::serve(stream, client, provider, ledger).await {
                    eprintln!("Native hook delivery retained: {error:#}");
                },
                Err(error) => return Err(error.into()),
            }
        }
        _ = tokio::time::sleep(Duration::from_secs(2)) => (),
    }
    Ok(())
}

async fn refresh(
    client: &Client,
    ledger: &Ledger,
    last: &mut Option<tokio::time::Instant>,
) -> Result<()> {
    if last.is_none_or(|at| at.elapsed() >= Duration::from_secs(30)) {
        report(client, ledger, InputReport::Ready).await?;
        *last = Some(tokio::time::Instant::now());
    }
    Ok(())
}

pub async fn run(client: Client, socket: Option<PathBuf>, args: Args) -> Result<()> {
    let client = client.with_start_timeout(None);
    let home = dirs::home();
    let binding = Binding {
        agent: args.agent,
        provider: ProviderGeneration {
            process: ProcessIdentity {
                pid: args.pid,
                started_at: args.started_at,
            },
            session: args.thread,
            profile: args
                .profile
                .canonicalize()?
                .to_str()
                .context("provider profile is not UTF-8")?
                .into(),
        },
        socket: socket.unwrap_or_else(|| dirs::socket_path(&home)),
        cwd: args.cwd.canonicalize()?,
        executable: args.program.canonicalize()?,
    };
    ensure!(
        procinfo::executable_path_of(binding.provider.process.pid)?.canonicalize()?
            == binding.executable,
        "native queue executable differs from the live Codex process"
    );
    identity(&client, &binding).await?;
    if let Some(predecessor) = args.predecessor {
        preflight(&binding).await?;
        resume::handoff(&client, binding, &predecessor).await?;
        // The accepted handoff registered the canonical launch descriptor.
        // Let the daemon start it, rather than competing with its next tick
        // for the retained ledger's lifetime lock.
        return Ok(());
    }
    let agent = identity(&client, &binding).await?;
    let mut ledger = Ledger::open(&home, binding.clone(), agent.input_binding.as_ref())?;
    let hooks = hooks::Listener::bind(&home, &binding.agent)?;
    // Prove the read-only native queue/history APIs before suppressing legacy
    // delivery. A missing API on an unbound session leaves hooks working. An
    // already bound session retains its queue and reports the incompatibility.
    if let Err(error) = preflight(&binding).await {
        if agent.input_binding.is_some() {
            let _ = report(&client, &ledger, crate::input_status::paused(&error)).await;
        }
        return Err(error);
    }
    let controller = ProcessIdentity {
        pid: std::process::id(),
        started_at: procinfo::start_time(std::process::id())
            .context("controller process birth is unavailable")?,
    };
    let launch = bootstrap::launch(&binding)?;
    loop {
        ensure!(alive(&binding), "provider exited before input binding");
        let request = Request::BindInput {
            agent: binding.agent.clone(),
            provider: binding.provider.clone(),
            controller: controller.clone(),
            token: ledger.record().token.clone(),
            launch: Some(launch.clone()),
        };
        match tokio::time::timeout(Duration::from_secs(5), client.call_raw(&request)).await {
            Ok(Ok(Response::InputBound {
                agent,
                binding: accepted,
                ..
            })) => {
                ensure!(
                    agent.as_str() == binding.agent
                        && accepted.provider == binding.provider
                        && accepted.controller == controller,
                    "daemon did not accept exact native queue ownership"
                );
                break;
            }
            Ok(Ok(Response::Error { .. })) => {
                bail!("daemon refused native queue binding; existing ownership is retained")
            }
            Ok(Ok(_)) => bail!("unexpected native queue binding response"),
            _ => tokio::time::sleep(Duration::from_secs(2)).await,
        }
        // The token was persisted before the first attempt. A lost successful
        // reply can safely retry this idempotent bind in the same process.
    }
    while alive(&binding) {
        let result = async {
            let mut provider = Provider::start_profile(
                &binding.executable,
                &[],
                &binding.cwd,
                Some(Path::new(&binding.provider.profile)),
            )?;
            let result = async {
                verify_provider(&mut provider, &binding).await?;
                service(&client, &mut provider, &mut ledger, &hooks).await
            }
            .await;
            let shutdown = provider.shutdown().await;
            result.and(shutdown)
        }
        .await;
        if let Err(error) = &result {
            let _ = report(&client, &ledger, crate::input_status::paused(error)).await;
            if let Some(failure) = error.downcast_ref::<crate::provider_status::Failure>()
                && failure.0.kind != agentdocker_core::ProviderIssueKind::Unknown
            {
                let _ = crate::provider_status::report(
                    &client,
                    &agent,
                    agentdocker_core::ProviderReport::Blocked {
                        issue: failure.0.clone(),
                    },
                )
                .await;
            }
            eprintln!("Native Codex input paused: {error}");
        }
        if !alive(&binding) {
            break;
        }
        // Retry read-only reconciliation after a transient disconnect, with
        // the same retained attempt and token. Never repeat queue/add.
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
    Ok(())
}
