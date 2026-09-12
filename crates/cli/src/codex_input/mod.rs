//! One supervised Codex conversation, fed by the daemon's ordinary Send queue.
mod config;
mod ledger;
mod recovery;
mod requests;
mod transport;

use crate::client::Client;
use agentdocker_core::{
    ActivityObservation, AgentRecord, Envelope, HUMAN, ReportedActivity, Request, Response,
};
use agentdocker_host::{dirs, procinfo, provider_input};
use anyhow::{Context, Result, bail, ensure};
use ledger::{Binding, Ledger};
use serde_json::{Value, json};
use std::{io::Write, path::PathBuf, time::Duration};
use tokio::{
    io::BufReader,
    task::JoinSet,
    time::{interval, timeout},
};
use transport::Provider;

#[derive(clap::Args)]
pub struct Args {
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

async fn call(client: &Client, request: Request) -> Result<Response> {
    timeout(Duration::from_secs(5), client.call(&request))
        .await
        .context("AgentDocker input queue did not respond")?
}

async fn identity(client: &Client, agent: &str, cwd: &std::path::Path) -> Result<AgentRecord> {
    // The supervisor records the child immediately after spawn. Give that one
    // transition a bounded opportunity to finish before proving ownership.
    for _ in 0..20 {
        let Response::Agent { agent } = call(
            client,
            Request::Inspect {
                agent: agent.into(),
            },
        )
        .await?
        else {
            bail!("Codex input owner is unavailable");
        };
        if agent.pid == Some(std::process::id()) {
            ensure!(
                provider_input::is_codex_input(&agent)
                    && agent.status.is_live()
                    && agent.process_started_at.is_some()
                    && agent.process_started_at == procinfo::start_time(std::process::id())
                    && agent
                        .spec
                        .workdir
                        .as_ref()
                        .and_then(|p| p.canonicalize().ok())
                        .as_deref()
                        == Some(cwd),
                "Codex input requires its exact live supervised identity and checkout"
            );
            return Ok(agent);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    bail!("Codex input is not running as the registered supervised process")
}

pub async fn run(client: Client, socket: Option<PathBuf>, args: Args) -> Result<()> {
    ensure!(
        std::env::var(provider_input::CODEX_INPUT_ENV).as_deref() == Ok("1"),
        "launch this controller with agentdocker run --runtime codex --codex-input"
    );
    let client = client.with_start_timeout(None);
    let home = dirs::home();
    let socket = socket.unwrap_or_else(|| agentdocker_core::paths::socket_path(&home));
    let cwd = std::env::current_dir()?.canonicalize()?;
    let agent_id =
        std::env::var("AGENTDOCKER_AGENT_ID").context("Codex input has no supervised identity")?;
    let agent = identity(&client, &agent_id, &cwd).await?;
    let provider_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".codex")))
        .context("Codex provider profile cannot be located")?
        .canonicalize()
        .context("Codex provider profile must already exist")?;
    let mut ledger = Ledger::open(
        &home,
        Binding {
            agent: agent.id.to_string(),
            socket,
            cwd: cwd.clone(),
            provider_home,
        },
    )?;
    let arguments = provider_input::codex_arguments(&args.command[1..])?;
    let mut provider = Provider::start(std::path::Path::new(&args.command[0]), &arguments, &cwd)?;
    let result = session(&client, &agent, &mut provider, &mut ledger).await;
    if let Err(error) = &result {
        eprintln!(
            "Codex input paused: {error:#}. Retained input will not be automatically submitted again."
        );
    }
    let shutdown = provider.shutdown().await;
    result.and(shutdown)
}

async fn queue(
    client: &Client,
    ledger: &Ledger,
    acknowledge: Vec<agentdocker_core::MessageId>,
) -> Result<Vec<Envelope>> {
    match call(
        client,
        Request::ProviderInbox {
            agent: ledger.record().binding.agent.clone(),
            acknowledge,
        },
    )
    .await?
    {
        Response::Messages { messages } => Ok(messages),
        _ => bail!(
            "daemon does not support the Codex input queue; update it before launching this mode"
        ),
    }
}

async fn acknowledge(client: &Client, ledger: &mut Ledger) -> Result<()> {
    let attempt = ledger
        .record()
        .attempt
        .as_ref()
        .context("no Codex input to acknowledge")?;
    ensure!(
        attempt.receipt.is_some(),
        "Codex has not confirmed the complete input"
    );
    let message = attempt.message.clone();
    queue(client, ledger, vec![message.clone().into()]).await?;
    ledger.acknowledge(&message)
}

async fn activity(client: &Client, agent: &str, activity: ReportedActivity) -> Result<()> {
    ensure!(
        matches!(
            call(
                client,
                Request::ReportActivity {
                    agent: agent.into(),
                    observation: ActivityObservation {
                        activity,
                        observed_at: chrono::Utc::now()
                    }
                }
            )
            .await?,
            Response::Ok
        ),
        "daemon refused Codex activity"
    );
    Ok(())
}

async fn preflight(provider: &mut Provider, cwd: &std::path::Path) -> Result<Value> {
    // Read the effective provider configuration and hook discovery. Do not
    // rewrite user/project profiles or turn off hooks, MCP, trust or approvals.
    let config = provider
        .request("config/read", json!({"cwd":cwd,"includeLayers":true}))
        .await?;
    ensure!(
        config.get("config").is_some_and(Value::is_object),
        "Codex effective configuration is unavailable"
    );
    let hooks = provider
        .request("hooks/list", json!({"cwds":[cwd]}))
        .await?;
    let entries = hooks["data"]
        .as_array()
        .context("Codex hook discovery is unavailable")?;
    ensure!(
        entries.len() == 1 && entries[0]["cwd"].as_str() == cwd.to_str(),
        "Codex hook discovery returned another checkout"
    );
    ensure!(
        entries[0]["errors"].as_array().is_some_and(Vec::is_empty),
        "Codex hook configuration has errors; resolve them before starting queued input"
    );
    Ok(config["config"].clone())
}

async fn session(
    client: &Client,
    agent: &AgentRecord,
    provider: &mut Provider,
    ledger: &mut Ledger,
) -> Result<()> {
    // Checking this before starting a thread keeps old daemons from launching
    // an input mode whose consumer reservation they do not understand.
    queue(client, ledger, Vec::new()).await?;
    provider.initialize().await?;
    let effective = preflight(provider, &ledger.record().binding.cwd).await?;
    let overrides = config::overrides(
        &effective,
        &ledger.record().binding,
        &dirs::home(),
        &procinfo::executable_path()?,
    )?;
    let resumed = ledger.record().thread.is_some();
    let response = if let Some(thread) = &ledger.record().thread {
        provider
            .request(
                "thread/resume",
                json!({"threadId":thread,"excludeTurns":true,"config":overrides}),
            )
            .await?
    } else {
        provider
            .request(
                "thread/start",
                json!({"cwd":ledger.record().binding.cwd,"config":overrides}),
            )
            .await?
    };
    let thread = response["thread"]["id"]
        .as_str()
        .context("Codex returned no conversation ID")?
        .to_owned();
    ensure!(
        response["thread"]["cwd"]
            .as_str()
            .and_then(|p| std::fs::canonicalize(p).ok())
            .as_ref()
            == Some(&ledger.record().binding.cwd),
        "Codex conversation has another checkout"
    );
    ledger.bind_thread(thread.clone())?;
    if resumed {
        recovery::recover(provider, client, ledger).await?;
    }
    let human = match call(
        client,
        Request::Me {
            workdir: Some(ledger.record().binding.cwd.clone()),
        },
    )
    .await?
    {
        Response::Agent { agent } => agent.id.to_string(),
        _ => bail!("human question routing is unavailable"),
    };
    activity(client, agent.id.as_str(), ReportedActivity::Idle).await?;
    println!("Codex ready. Send a message here or from AgentDocker.");
    let mut poll = interval(Duration::from_millis(500));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut input = Vec::new();
    let mut input_open = agent.spec.tty || agent.spec.in_pane;
    let mut turn: Option<String> = None;
    let mut answers = JoinSet::new();
    let mut request_ids = std::collections::HashSet::new();
    loop {
        tokio::select! {
            event = provider.next() => {
                let event = event?;
                if event.get("method").is_some() && event.get("id").is_some() {
                    ensure!(answers.len() < 8, "too many outstanding Codex requests");
                    let key = event["id"].to_string();
                    ensure!(request_ids.insert(key.clone()), "Codex repeated an outstanding request ID");
                    let client = client.clone(); let human = human.clone(); let agent_id = agent.id.to_string();
                    let thread = thread.clone(); let turn = turn.clone();
                    answers.spawn(async move { (key, requests::answer(client, agent_id, human, thread, turn, event).await) });
                    continue;
                }
                let params = &event["params"];
                if params.get("threadId").and_then(Value::as_str) != Some(&thread) { continue; }
                match event["method"].as_str() {
                    Some("item/started" | "item/completed") if params["item"]["type"] == "userMessage" => {
                        let expected = turn.as_deref().context("Codex supplied an unexpected input receipt")?;
                        let attempt = ledger.record().attempt.as_ref().context("Codex input receipt has no pending message")?;
                        if let Some(receipt) = recovery::receipt(&thread, params["turnId"].as_str().unwrap_or_default(), &params["item"], &attempt.input)? {
                            ensure!(receipt.turn == expected, "Codex input receipt has another turn");
                            let input = attempt.input.clone(); ledger.accept(&input, receipt)?;
                            acknowledge(client, ledger).await?;
                        } else { bail!("Codex supplied a different input while this controller owned the turn"); }
                    }
                    Some("item/agentMessage/delta") => {
                        if let Some(delta) = params["delta"].as_str() { print!("{delta}"); std::io::stdout().flush()?; }
                    }
                    Some("turn/completed") => {
                        let id = params["turn"]["id"].as_str().context("Codex completed turn has no ID")?;
                        ensure!(turn.as_deref() == Some(id), "Codex completed an unexpected turn");
                        if ledger.record().attempt.as_ref().is_some_and(|a| a.receipt.is_none()) {
                            recovery::find_receipt(provider, ledger).await?;
                        }
                        acknowledge(client, ledger).await?;
                        let status = params["turn"]["status"].as_str().context("Codex completed turn has no status")?;
                        ensure!(recovery::terminal(status), "Codex completed notification is not terminal");
                        ledger.finish(id)?; turn = None;
                        // A provider may finish after cancelling an approval.
                        // A late human answer must not grant that stale callback.
                        answers.abort_all(); request_ids.clear();
                        println!("\nCodex turn {status}.");
                        activity(client, agent.id.as_str(), ReportedActivity::Idle).await?;
                    }
                    Some("error") => eprintln!("Codex reported an error; waiting for the turn outcome."),
                    _ => (),
                }
            }
            answer = answers.join_next(), if !answers.is_empty() => {
                let answer = answer.context("Codex request worker disappeared")?;
                if answer.as_ref().is_err_and(|error| error.is_cancelled()) { continue; }
                let (key, response) = answer?;
                request_ids.remove(&key);
                provider.send(&response).await?;
            }
            line = transport::read_frame(&mut stdin, &mut input), if input_open => {
                match line {
                    Ok(bytes) => {
                        let text = String::from_utf8(bytes).context("input must be UTF-8")?;
                        let text = text.trim_end_matches(['\r', '\n']);
                        if !text.is_empty() {
                            ensure!(text.len() <= 16_000, "message exceeds 16000 bytes");
                            ensure!(matches!(call(client, Request::Send { from: HUMAN.into(), to: agent.id.to_string(),
                                kind: "chat".into(), payload: json!({"text":text}), reply_to: None }).await?, Response::Sent { .. }), "message was not queued");
                        }
                    }
                    Err(error) => { input_open = false; eprintln!("Terminal input closed: {error}"); }
                }
            }
            _ = poll.tick(), if turn.is_none() && answers.is_empty() => {
                if let Some(message) = queue(client, ledger, Vec::new()).await?.first() {
                    preflight(provider, &ledger.record().binding.cwd).await?;
                    let input = ledger.prepare(message)?;
                    activity(client, agent.id.as_str(), ReportedActivity::Working).await?;
                    let result = provider.request("turn/start", json!({"threadId":thread,
                        "input":[{"type":"text","text":input,"text_elements":[]}]})).await?;
                    turn = Some(result["turn"]["id"].as_str().context("Codex accepted no identifiable turn")?.to_owned());
                }
            }
        }
    }
}
