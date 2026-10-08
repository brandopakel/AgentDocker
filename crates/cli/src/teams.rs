//! `agentdocker up` / `down`: start and stop the agents in an Agentfile;
//! `agentdocker agentfile`: check, upgrade and describe one.

use std::path::Path;

use agentdocker_core::{AgentRecord, AgentSpec, Request, Response};
use anyhow::{Context, Result, bail};

use crate::agentfile::Agentfile;
use crate::client::Client;

/// Start every agent in the file (or just `only`) that is not already live.
/// Prints the id of each agent it started on stdout, one per line, so scripts
/// can capture them; progress goes to stderr.
pub async fn up(client: &Client, file: Option<&Path>, only: &[String]) -> Result<()> {
    let (agentfile, path) = Agentfile::load(file)?;
    let built = agentfile.specs(&path, only, &crate::agentfile::SystemHost::new())?;
    for note in built.iter().flat_map(|(_, notes)| notes) {
        eprintln!("note: {note}");
    }
    let specs = order(built.into_iter().map(|(spec, _)| spec).collect());
    let live = live_agents(client).await?;
    let mut failed = 0;
    'agents: for spec in specs {
        let name = spec.name.clone();
        if let Some(existing) = live.iter().find(|a| a.spec.name == name) {
            eprintln!("{name:<24} already running   {}", existing.id.short());
            continue;
        }
        // Ordering alone is not enough: a dependency that is `created`
        // has not run its first line yet, and the agent about to start
        // may be a client of it. Wait for it to actually be running; an
        // agent whose dependency never came up is not started at all.
        for needed in &spec.depends_on {
            if let Err(err) = wait_for(client, needed).await {
                eprintln!("{name:<24} failed: {err:#}");
                failed += 1;
                continue 'agents;
            }
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

/// Dependencies first.
///
/// The file is validated when it is read — no self-dependency, no name
/// that is not in the file, no cycle — so this is a plain depth-first
/// walk that cannot loop. Order within the file is preserved for
/// everything a dependency does not decide, because that is the order
/// somebody wrote them in.
pub(crate) fn order(specs: Vec<AgentSpec>) -> Vec<AgentSpec> {
    let mut ordered: Vec<AgentSpec> = Vec::with_capacity(specs.len());
    let mut placed: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Depth capped at the number of agents: the file is acyclic, so the
    // longest chain is every agent once, and the cap is belt and braces
    // for a file that reached here another way.
    fn place(
        name: &str,
        specs: &[AgentSpec],
        ordered: &mut Vec<AgentSpec>,
        placed: &mut std::collections::HashSet<String>,
        depth: usize,
    ) {
        if depth == 0 || placed.contains(name) {
            return;
        }
        let Some(spec) = specs.iter().find(|s| s.name == name) else {
            return;
        };
        placed.insert(name.to_owned());
        for needed in &spec.depends_on {
            place(needed, specs, ordered, placed, depth - 1);
        }
        ordered.push(spec.clone());
    }
    let names: Vec<String> = specs.iter().map(|s| s.name.clone()).collect();
    for name in &names {
        place(name, &specs, &mut ordered, &mut placed, specs.len());
    }
    ordered
}

/// Wait until an agent is running, or say why it never will be.
async fn wait_for(client: &Client, name: &str) -> Result<()> {
    let deadline = std::time::Instant::now() + DEPENDENCY_TIMEOUT;
    loop {
        let live = live_agents(client).await?;
        match live.iter().find(|a| a.spec.name == name) {
            Some(agent) if agent.status == agentdocker_core::AgentStatus::Running => {
                return Ok(());
            }
            Some(_) => {}
            None => {}
        }
        if std::time::Instant::now() >= deadline {
            bail!("`{name}` did not start within {DEPENDENCY_TIMEOUT:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// How long `up` waits for a dependency before giving up on it. Long
/// enough for a server to bind a port, short enough that a typo does not
/// hang the terminal.
const DEPENDENCY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Stop every live agent named in the file (or just `only`).
pub async fn down(
    client: &Client,
    file: Option<&Path>,
    only: &[String],
    force: bool,
) -> Result<()> {
    // Stopping needs names only: an unset `${VAR}` must not keep anyone
    // from stopping what is running.
    let (agentfile, path) = Agentfile::load(file)?;
    let names = agentfile.names(&path, only)?;
    let live = live_agents(client).await?;
    let mut failed = 0;
    for name in names {
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

/// `agentfile check`: read the file the way `up` will — every version
/// upgraded, `${VAR}` expanded, commands built — and show what would be
/// launched, without launching anything or asking the daemon. Environment
/// values are not shown, only their names: expansion can put a secret there.
pub fn check(file: Option<&Path>, only: &[String], json: bool) -> Result<()> {
    let (agentfile, path) = Agentfile::load(file)?;
    let built = agentfile.specs(&path, only, &crate::agentfile::SystemHost::new())?;
    if json {
        let rows: Vec<serde_json::Value> = order(built.iter().map(|(s, _)| s.clone()).collect())
            .into_iter()
            .map(|spec| {
                let notes = built
                    .iter()
                    .find(|(s, _)| s.name == spec.name)
                    .map(|(_, notes)| notes.clone())
                    .unwrap_or_default();
                serde_json::json!({
                    "name": spec.name,
                    "runtime": spec.runtime,
                    "model": spec.model,
                    "workdir": spec.workdir,
                    "command": spec.command,
                    "env": spec.env.keys().collect::<Vec<_>>(),
                    "tty": spec.tty,
                    "isolate": spec.isolate,
                    "depends_on": spec.depends_on,
                    "notes": notes,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    println!(
        "{} (version {} as read; {} agent(s))",
        path.display(),
        agentfile.version,
        built.len()
    );
    for spec in order(built.iter().map(|(s, _)| s.clone()).collect()) {
        let notes = &built
            .iter()
            .find(|(s, _)| s.name == spec.name)
            .expect("built")
            .1;
        println!(
            "\n{}  {}{}",
            spec.name,
            spec.runtime,
            spec.model
                .as_deref()
                .map(|m| format!("  model {m}"))
                .unwrap_or_default()
        );
        if let Some(dir) = &spec.workdir {
            println!("  in   {}", dir.display());
        }
        // A wired-in settings or MCP document is long JSON; its size says
        // enough here, and `--json` shows it whole.
        let words: Vec<String> = spec
            .command
            .iter()
            .map(|word| {
                if word.starts_with('{') && word.len() > 60 {
                    format!("<{} bytes of JSON>", word.len())
                } else {
                    word.clone()
                }
            })
            .collect();
        println!(
            "  runs {}",
            shlex::try_join(words.iter().map(String::as_str)).unwrap_or_else(|_| words.join(" "))
        );
        if !spec.env.is_empty() {
            println!(
                "  env  {}",
                spec.env.keys().cloned().collect::<Vec<_>>().join(", ")
            );
        }
        if !spec.depends_on.is_empty() {
            println!("  after {}", spec.depends_on.join(", "));
        }
        for note in notes {
            println!("  note {note}");
        }
    }
    Ok(())
}

/// `agentfile upgrade`: the file as the latest version, on stdout. The
/// file itself is left alone; comments do not survive the round trip, so
/// the person decides what to keep.
pub fn upgrade(file: Option<&Path>) -> Result<()> {
    let (agentfile, path) = Agentfile::load(file)?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let declared = text
        .parse::<toml::Table>()
        .ok()
        .and_then(|table| table.get("version").and_then(toml::Value::as_integer))
        .unwrap_or(1);
    print!("{}", agentfile.render()?);
    if declared == i64::from(crate::agentfile::LATEST) {
        eprintln!(
            "{} is already version {}; printed as it reads (comments are not kept)",
            path.display(),
            crate::agentfile::LATEST
        );
    } else {
        eprintln!(
            "{} upgraded from version {declared} to {}: every `$` is doubled so it still means itself. Comments are not kept; compare before replacing the file.",
            path.display(),
            crate::agentfile::LATEST
        );
    }
    Ok(())
}

async fn live_agents(client: &Client) -> Result<Vec<AgentRecord>> {
    match client
        .call(&Request::List {
            all: false,
            project: None,
            labels: Default::default(),
        })
        .await?
    {
        Response::Agents { agents, .. } => Ok(agents),
        other => bail!("unexpected reply to list: {other:?}"),
    }
}
