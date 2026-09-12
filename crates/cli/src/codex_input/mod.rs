//! One supervised Codex conversation, fed by the daemon's ordinary Send queue.
mod config;
mod daemon_io;
mod file_changes;
mod ledger;
mod mcp_answers;
mod question_events;
mod recovery;
mod requests;
mod review;
mod terminal;
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
    let result = run_owned(&client, args, home, socket, cwd, &agent).await;
    if let Err(cause) = &result {
        if let Err(error) = crate::input_status::report(
            &client,
            agent.id.as_str(),
            agent.process_started_at,
            crate::input_status::paused(cause),
        )
        .await
        {
            eprintln!("Could not persist paused input status: {error:#}");
        }
    }
    result
}

async fn run_owned(
    client: &Client,
    args: Args,
    home: PathBuf,
    socket: PathBuf,
    cwd: PathBuf,
    agent: &AgentRecord,
) -> Result<()> {
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
    requests::recover(client, &mut ledger).await?;
    mcp_answers::acknowledge(client, &mut ledger).await?;
    let arguments = provider_input::codex_arguments(&args.command[1..])?;
    let mut provider = Provider::start(std::path::Path::new(&args.command[0]), &arguments, &cwd)?;
    let result = session(client, agent, &mut provider, &mut ledger).await;
    if let Err(error) = &result {
        eprintln!(
            "Codex input paused: {error:#}. Retained input will not be automatically submitted again."
        );
    }
    let shutdown = provider.shutdown().await;
    if result.is_err() || shutdown.is_err() {
        if let Err(error) = requests::cancel_pending(client, &ledger).await {
            eprintln!("Could not close retained Codex questions: {error:#}");
        }
    }
    result.and(shutdown)
}

async fn queue(
    client: &Client,
    ledger: &Ledger,
    acknowledge: Vec<agentdocker_core::MessageId>,
) -> Result<Vec<Envelope>> {
    match daemon_io::queue(client, &ledger.record().binding.agent, acknowledge).await? {
        Response::Messages { messages } => Ok(messages),
        _ => bail!(
            "daemon does not support the Codex input queue; update it before launching this mode"
        ),
    }
}

async fn acknowledge(client: &Client, ledger: &mut Ledger, agent: &AgentRecord) -> Result<()> {
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
    let receipt = attempt.receipt.as_ref().expect("checked receipt");
    crate::input_status::report(
        client,
        agent.id.as_str(),
        agent.process_started_at,
        agentdocker_core::InputReport::Received {
            input: agentdocker_core::ReceivedInput {
                messages: vec![message.clone().into()],
                receipt: agentdocker_core::InputReceipt::Codex {
                    thread: receipt.thread.clone(),
                    turn: receipt.turn.clone(),
                    item: receipt.item.clone(),
                },
            },
        },
    )
    .await?;
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
    let cwd = cwd.to_str().context("Codex checkout path is not UTF-8")?;
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
        entries.len() == 1 && entries[0]["cwd"].as_str() == Some(cwd),
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
    // Codex does not persist an empty thread's rollout. Only a controller that
    // has never prepared input may replace that unused handle after restart.
    let resumed = ledger.record().attempt.is_some() || !ledger.record().completed.is_empty();
    if !resumed {
        ledger.discard_unused_thread()?;
    }
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
        recovery::recover(provider, client, ledger, agent).await?;
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
    let mcp_origin = mcp_answers::Origin::from_overrides(human.clone(), &overrides)?;
    let mut question_events = question_events::Events::start(client.clone()).await?;
    activity(client, agent.id.as_str(), ReportedActivity::Idle).await?;
    crate::input_status::report(
        client,
        agent.id.as_str(),
        agent.process_started_at,
        agentdocker_core::InputReport::Ready,
    )
    .await?;
    println!("Codex ready. Send a message here or from AgentDocker.");
    let mut poll = interval(Duration::from_millis(500));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut heartbeat = interval(Duration::from_secs(30));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut input = terminal::Input::default();
    let mut input_open = agent.spec.tty || agent.spec.in_pane;
    let mut turn: Option<String> = None;
    let mut request_ids = std::collections::HashSet::new();
    let mut file_reviews = file_changes::Reviews::default();
    loop {
        tokio::select! {
            event = question_events.next() => { requests::observe(ledger, &event?)?; }
            _ = heartbeat.tick() => {
                activity(client, agent.id.as_str(), if turn.is_some() {
                    ReportedActivity::Working
                } else { ReportedActivity::Idle }).await?;
            }
            event = provider.next() => {
                let event = event?;
                if event.get("method").is_some() && event.get("id").is_some() {
                    let key = event["id"].to_string();
                    ensure!(request_ids.len() < 10_000 && request_ids.insert(key), "Codex repeated a request ID or exceeded the request history bound");
                    if let Some(response) = requests::open(client, ledger, &human, &thread, turn.as_deref(), event, &mut file_reviews).await? {
                        provider.send(&response).await?;
                    }
                    continue;
                }
                let params = &event["params"];
                if params.get("threadId").and_then(Value::as_str) != Some(&thread) { continue; }
                file_reviews.observe(&event, &thread, turn.as_deref());
                file_reviews.check_current()?;
                match event["method"].as_str() {
                    Some("item/completed") if params["item"]["type"] == "mcpToolCall" => {
                        mcp_answers::observe(client, ledger, &thread, params["turnId"].as_str().unwrap_or_default(), &params["item"]).await?;
                    }
                    Some("serverRequest/resolved") => requests::resolved(client, ledger, params).await?,
                    Some("item/started" | "item/completed") if params["item"]["type"] == "userMessage" => {
                        let expected = turn.as_deref().context("Codex supplied an unexpected input receipt")?;
                        let attempt = ledger.record().attempt.as_ref().context("Codex input receipt has no pending message")?;
                        if let Some(receipt) = recovery::receipt(&thread, params["turnId"].as_str().unwrap_or_default(), &params["item"], &attempt.input)? {
                            ensure!(receipt.turn == expected, "Codex input receipt has another turn");
                            let input = attempt.input.clone(); ledger.accept(&input, receipt)?;
                            acknowledge(client, ledger, agent).await?;
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
                        acknowledge(client, ledger, agent).await?;
                        let status = params["turn"]["status"].as_str().context("Codex completed turn has no status")?;
                        ensure!(recovery::terminal(status), "Codex completed notification is not terminal");
                        requests::turn_ended(client, ledger).await?;
                        mcp_answers::reconcile(provider, client, ledger).await?;
                        ledger.finish(id)?; turn = None;
                        file_reviews = file_changes::Reviews::default();
                        println!("\nCodex turn {status}.");
                        activity(client, agent.id.as_str(), ReportedActivity::Idle).await?;
                    }
                    Some("error") => eprintln!("Codex reported an error; waiting for the turn outcome."),
                    _ => (),
                }
            }
            line = input.read(&mut stdin), if input_open => {
                match line {
                    Ok(Some(text)) => {
                        ensure!(matches!(call(client, Request::Send { from: HUMAN.into(), to: agent.id.to_string(),
                            kind: "chat".into(), payload: json!({"text":text}), reply_to: None }).await?, Response::Sent { .. }), "message was not queued");
                    }
                    Ok(None) => (),
                    Err(error) => { input_open = false; eprintln!("Terminal input closed: {error}"); }
                }
            }
            _ = poll.tick() => {
                let messages = requests::poll(client, provider, ledger).await?;
                if turn.is_none() && ledger.record().reviews.is_empty() {
                if let Some(message) = messages.first() {
                    preflight(provider, &ledger.record().binding.cwd).await?;
                    let input = ledger.prepare_bound(message, Some(mcp_origin.clone()))?;
                    activity(client, agent.id.as_str(), ReportedActivity::Working).await?;
                    let result = provider.request("turn/start", json!({"threadId":thread,
                        "input":[{"type":"text","text":input,"text_elements":[]}]})).await?;
                    turn = Some(result["turn"]["id"].as_str().context("Codex accepted no identifiable turn")?.to_owned());
                }
                }
            }
        }
    }
}
