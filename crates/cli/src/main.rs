//! `agentdocker`: command-line client for `agentd`.

mod agentfile;
mod client;
mod format;
mod hooks;
mod mcp;
mod teams;

use std::collections::BTreeMap;
use std::path::PathBuf;

use agentdocker_core::{
    AgentRecord, AgentSpec, Lease, LeaseId, LeaseMode, MessageId, Request, Response,
    protocol::DEFAULT_LEASE_TTL_SECS,
};
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};

use crate::client::Client;

#[derive(Parser)]
#[command(
    name = "agentdocker",
    version,
    about = "Docker-style control plane for AI agents",
    propagate_version = true
)]
struct Cli {
    /// Socket of the agentd daemon.
    #[arg(long, global = true, env = "AGENTDOCKER_SOCKET")]
    socket: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check that agentd is reachable.
    Ping,
    /// List agents (live ones by default), grouped by project.
    Ps {
        /// Include finished agents.
        #[arg(short, long)]
        all: bool,
        /// Only agents in this project: an id (or unique prefix), or a path
        /// inside it such as `.`.
        #[arg(long, value_name = "ID|PATH")]
        project: Option<String>,
        /// Only agents carrying this label (repeatable).
        #[arg(short = 'l', long = "label", value_name = "KEY=VALUE")]
        labels: Vec<String>,
    },
    /// Launch a command as a supervised agent and print its id.
    Run(RunArgs),
    /// Announce an already-running process as an agent and print its id.
    Register(RegisterArgs),
    /// Mark an externally managed agent as finished.
    Deregister {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        agent: String,
    },
    /// Signal an agent to stop.
    Stop {
        agent: String,
        /// SIGKILL instead of SIGTERM.
        #[arg(short, long)]
        force: bool,
    },
    /// Forget a finished agent.
    Rm { agent: String },
    /// Show everything known about an agent, as JSON.
    Inspect { agent: String },
    /// Show an agent's captured output.
    Logs {
        agent: String,
        /// Keep streaming until the agent exits.
        #[arg(short, long)]
        follow: bool,
        /// Lines to replay first; 0 for all.
        #[arg(long, default_value_t = 100)]
        tail: usize,
    },
    /// Report that an agent is alive.
    Heartbeat {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        agent: String,
    },
    /// Send a message to an agent, this project (`project`), a topic (`topic:name`), or everyone (`all`).
    Send(SendArgs),
    /// Stream messages for an agent and/or matching topic patterns.
    Watch {
        /// Receive messages addressed to this agent.
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        agent: Option<String>,
        /// Topic patterns, e.g. `repo/#` or `reviews/+/done`.
        topics: Vec<String>,
    },
    /// Show messages queued for an agent while it was not watching.
    Inbox {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        agent: String,
        /// Remove the messages after showing them.
        #[arg(long)]
        drain: bool,
    },
    /// Claim a lease on a resource (`path:...`, `branch:...`, `task:...`).
    Claim(ClaimArgs),
    /// Extend a lease you hold.
    Renew {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        agent: String,
        lease: String,
        #[arg(long, default_value_t = DEFAULT_LEASE_TTL_SECS)]
        ttl: u64,
    },
    /// Release a lease you hold, or every lease with --all.
    Release {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        agent: String,
        #[arg(required_unless_present = "all")]
        lease: Option<String>,
        /// Release every lease this agent holds.
        #[arg(long, conflicts_with = "lease")]
        all: bool,
    },
    /// List leases.
    Leases {
        /// Only leases held by this agent.
        #[arg(long)]
        agent: Option<String>,
        /// Only leases overlapping this resource.
        #[arg(long)]
        resource: Option<String>,
    },
    /// Host integrations: handle a hook event, or install the hook configuration.
    Hook(hooks::HookArgs),
    /// Serve AgentDocker's tools to an MCP host (Claude Code, Codex, Cursor...) over stdio.
    Mcp(mcp::McpArgs),
    /// Start the agents in an Agentfile.toml that are not already running.
    Up {
        /// Agentfile to read (default: ./Agentfile.toml).
        #[arg(short = 'f', long)]
        file: Option<PathBuf>,
        /// Only these agents.
        names: Vec<String>,
    },
    /// Stop the agents in an Agentfile.toml.
    Down {
        /// Agentfile to read (default: ./Agentfile.toml).
        #[arg(short = 'f', long)]
        file: Option<PathBuf>,
        /// Only these agents.
        names: Vec<String>,
        /// SIGKILL instead of SIGTERM.
        #[arg(long)]
        force: bool,
    },
    /// Stream daemon events.
    Events {
        /// Show this many stored events before streaming new ones.
        #[arg(long, default_value_t = 0)]
        replay: usize,
    },
}

#[derive(Args)]
struct RunArgs {
    /// Agent name (default: generated).
    #[arg(long)]
    name: Option<String>,
    /// Runtime hosting the agent: claude-code, codex, gemini-cli, cursor, custom...
    #[arg(long, default_value = "custom")]
    runtime: String,
    #[arg(long)]
    provider: Option<String>,
    #[arg(long)]
    model: Option<String>,
    /// Working directory (default: current directory).
    #[arg(short = 'w', long)]
    workdir: Option<PathBuf>,
    /// Environment variable for the agent process.
    #[arg(short = 'e', long = "env", value_name = "KEY=VALUE")]
    env: Vec<String>,
    /// Label for organising agents.
    #[arg(short = 'l', long = "label", value_name = "KEY=VALUE")]
    labels: Vec<String>,
    /// Command to launch, after `--`.
    #[arg(required = true, last = true)]
    command: Vec<String>,
}

#[derive(Args)]
struct RegisterArgs {
    #[arg(long)]
    name: String,
    #[arg(long, default_value = "custom")]
    runtime: String,
    #[arg(long)]
    provider: Option<String>,
    #[arg(long)]
    model: Option<String>,
    /// Process id, so `stop` can signal it.
    #[arg(long)]
    pid: Option<u32>,
    /// Working directory (default: current directory).
    #[arg(short = 'w', long)]
    workdir: Option<PathBuf>,
    #[arg(short = 'l', long = "label", value_name = "KEY=VALUE")]
    labels: Vec<String>,
}

#[derive(Args)]
struct SendArgs {
    /// Sender; defaults to this agent's id, or `user`.
    #[arg(long, env = "AGENTDOCKER_AGENT_ID", default_value = "user")]
    from: String,
    /// Agent id/name, `project` (everyone working in this directory's
    /// project) or `project:<id|path>`, `topic:<name>`, or `all`.
    #[arg(long)]
    to: String,
    /// Message kind: chat, task, handoff, question, answer, notice...
    #[arg(long, default_value = "chat")]
    kind: String,
    /// Id of the message this replies to.
    #[arg(long)]
    reply_to: Option<String>,
    /// Message text, sent as {"text": ...}.
    text: Option<String>,
    /// Raw JSON payload instead of text.
    #[arg(long, conflicts_with = "text")]
    json: Option<String>,
}

#[derive(Args)]
struct ClaimArgs {
    #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
    agent: String,
    /// `kind:value`. A bare path becomes `path:<absolute>`; it need not exist yet.
    resource: String,
    /// Allow other shared holders; blocks exclusive ones.
    #[arg(long)]
    shared: bool,
    /// Seconds until the lease expires unless renewed.
    #[arg(long, default_value_t = DEFAULT_LEASE_TTL_SECS)]
    ttl: u64,
    /// What you are doing with it, for other agents to read.
    #[arg(long)]
    note: Option<String>,
    /// Seconds to wait for the resource to free up instead of failing at once.
    #[arg(long, default_value_t = 0)]
    wait: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = Client::new(cli.socket);

    match cli.command {
        Command::Ping => {
            if let Response::Pong {
                version,
                uptime_secs,
            } = client.call(&Request::Ping).await?
            {
                println!("agentd {version} up {}", format::span_secs(uptime_secs));
            }
        }
        Command::Ps {
            all,
            project,
            labels,
        } => {
            let request = Request::List {
                all,
                project: project.as_deref().map(project_selector),
                labels: parse_pairs(&labels)?,
            };
            if let Response::Agents { agents } = client.call(&request).await? {
                print_agents(&agents);
            }
        }
        Command::Run(args) => {
            let workdir = match args.workdir {
                Some(dir) => dir,
                None => std::env::current_dir()?,
            };
            let spec = AgentSpec {
                name: args.name.unwrap_or_default(),
                runtime: args.runtime,
                provider: args.provider,
                model: args.model,
                command: args.command,
                workdir: Some(workdir.canonicalize().unwrap_or(workdir)),
                env: parse_pairs(&args.env)?,
                labels: parse_pairs(&args.labels)?,
            };
            if let Response::Agent { agent } = client.call(&Request::Run { spec }).await? {
                println!("{}", agent.id);
            }
        }
        Command::Register(args) => {
            let workdir = match args.workdir {
                Some(dir) => dir,
                None => std::env::current_dir()?,
            };
            let spec = AgentSpec {
                name: args.name,
                runtime: args.runtime,
                provider: args.provider,
                model: args.model,
                command: Vec::new(),
                workdir: Some(workdir.canonicalize().unwrap_or(workdir)),
                env: BTreeMap::new(),
                labels: parse_pairs(&args.labels)?,
            };
            let request = Request::Register {
                spec,
                pid: args.pid,
            };
            if let Response::Agent { agent } = client.call(&request).await? {
                println!("{}", agent.id);
            }
        }
        Command::Deregister { agent } => {
            client.call(&Request::Deregister { agent }).await?;
        }
        Command::Stop { agent, force } => {
            if let Response::Agent { agent } = client.call(&Request::Stop { agent, force }).await? {
                println!("{}", agent.id);
            }
        }
        Command::Rm { agent } => {
            client.call(&Request::Remove { agent }).await?;
        }
        Command::Inspect { agent } => {
            if let Response::Agent { agent } = client.call(&Request::Inspect { agent }).await? {
                println!("{}", serde_json::to_string_pretty(&agent)?);
            }
        }
        Command::Logs {
            agent,
            follow,
            tail,
        } => {
            let request = Request::Logs {
                agent,
                follow,
                tail,
            };
            client
                .stream(&request, |response| {
                    if let Response::Log { line } = response {
                        println!("{line}");
                    }
                    Ok(true)
                })
                .await?;
        }
        Command::Heartbeat { agent } => {
            client.call(&Request::Heartbeat { agent }).await?;
        }
        Command::Send(args) => {
            let payload: Value = match (args.json, args.text) {
                (Some(raw), _) => serde_json::from_str(&raw).context("--json is not valid JSON")?,
                (None, Some(text)) => json!({ "text": text }),
                (None, None) => bail!("give message text or --json"),
            };
            let request = Request::Send {
                from: args.from,
                to: destination(&args.to),
                kind: args.kind,
                payload,
                reply_to: args.reply_to.map(MessageId::from),
            };
            if let Response::Sent {
                message,
                subscribers,
            } = client.call(&request).await?
            {
                println!("{message} ({subscribers} live subscriber(s))");
            }
        }
        Command::Watch { agent, topics } => {
            if agent.is_none() && topics.is_empty() {
                bail!("watch needs --as <agent> and/or topic patterns");
            }
            let request = Request::Subscribe { agent, topics };
            client
                .stream(&request, |response| {
                    if let Response::Message { message } = response {
                        println!("{}", format::message_line(&message));
                    }
                    Ok(true)
                })
                .await?;
        }
        Command::Inbox { agent, drain } => {
            if let Response::Messages { messages } =
                client.call(&Request::Inbox { agent, drain }).await?
            {
                for message in &messages {
                    println!("{}", format::message_line(message));
                }
            }
        }
        Command::Claim(args) => {
            let request = Request::Claim {
                agent: args.agent,
                resource: resource_key(&args.resource),
                mode: if args.shared {
                    LeaseMode::Shared
                } else {
                    LeaseMode::Exclusive
                },
                ttl_secs: args.ttl,
                note: args.note,
                wait_secs: args.wait,
            };
            if let Response::Lease { lease } = client.call(&request).await? {
                println!("{}", lease.id);
            }
        }
        Command::Renew { agent, lease, ttl } => {
            let request = Request::Renew {
                agent,
                lease: LeaseId::from(lease.as_str()),
                ttl_secs: ttl,
            };
            if let Response::Lease { lease } = client.call(&request).await? {
                println!("{} expires {}", lease.id, format::until(lease.expires_at));
            }
        }
        Command::Release { agent, lease, all } => {
            if all {
                if let Response::Leases { leases } =
                    client.call(&Request::ReleaseAll { agent }).await?
                {
                    for lease in leases {
                        println!("{}", lease.id);
                    }
                }
            } else if let Some(lease) = lease {
                let request = Request::Release {
                    agent,
                    lease: LeaseId::from(lease.as_str()),
                };
                client.call(&request).await?;
            }
        }
        Command::Leases { agent, resource } => {
            let request = Request::Leases {
                agent,
                resource: resource.as_deref().map(resource_key),
            };
            if let Response::Leases { leases } = client.call(&request).await? {
                print_leases(&leases);
            }
        }
        Command::Hook(args) => hooks::run(client, args).await?,
        Command::Mcp(args) => mcp::serve(client, args).await?,
        Command::Up { file, names } => teams::up(&client, file.as_deref(), &names).await?,
        Command::Down { file, names, force } => {
            teams::down(&client, file.as_deref(), &names, force).await?;
        }
        Command::Events { replay } => {
            client
                .stream(&Request::Events { replay }, |response| {
                    if let Response::Event { event } = response {
                        println!("{}", format::event_line(&event));
                    }
                    Ok(true)
                })
                .await?;
        }
    }
    Ok(())
}

/// `project` means the project containing the current directory; a
/// `project:` selector is normalised like `ps --project`; anything else
/// is passed through for the daemon to interpret.
pub(crate) fn destination(raw: &str) -> String {
    match raw {
        "project" => format!("project:{}", project_selector(".")),
        _ => match raw.strip_prefix("project:") {
            Some(selector) => format!("project:{}", project_selector(selector)),
            None => raw.to_owned(),
        },
    }
}

/// A project id prefix passes through; anything that names a path (`.`,
/// something with a slash, or an existing entry) becomes absolute so the
/// daemon can find the project containing it.
pub(crate) fn project_selector(raw: &str) -> String {
    let path = PathBuf::from(raw);
    if !(raw == "." || raw.contains('/') || path.exists()) {
        return raw.to_owned();
    }
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&path))
            .unwrap_or(path)
    };
    absolute
        .canonicalize()
        .unwrap_or(absolute)
        .to_string_lossy()
        .into_owned()
}

/// `repo` for the main checkout, `repo@wt` inside a linked worktree.
fn project_cell(agent: &AgentRecord) -> String {
    match &agent.project {
        None => "-".to_owned(),
        Some(project) => match &project.worktree {
            None => project.name(),
            Some(worktree) => format!(
                "{}@{}",
                project.name(),
                worktree
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
        },
    }
}

fn print_agents(agents: &[AgentRecord]) {
    let rows: Vec<Vec<String>> = agents
        .iter()
        .map(|a| {
            vec![
                a.id.short().to_owned(),
                a.spec.name.clone(),
                project_cell(a),
                a.spec.runtime.clone(),
                a.spec.model.clone().unwrap_or_else(|| "-".to_owned()),
                a.status.to_string(),
                a.pid
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "-".to_owned()),
                format::ago(a.created_at),
            ]
        })
        .collect();
    format::table(
        &[
            "AGENT ID", "NAME", "PROJECT", "RUNTIME", "MODEL", "STATUS", "PID", "CREATED",
        ],
        &rows,
    );
}

fn print_leases(leases: &[Lease]) {
    let rows: Vec<Vec<String>> = leases
        .iter()
        .map(|l| {
            vec![
                l.id.to_string(),
                l.resource.to_string(),
                l.holder.short().to_owned(),
                l.mode.to_string(),
                format::until(l.expires_at),
                l.note.clone().unwrap_or_default(),
            ]
        })
        .collect();
    format::table(
        &["LEASE ID", "RESOURCE", "HOLDER", "MODE", "EXPIRES", "NOTE"],
        &rows,
    );
}

/// Turn a user-supplied resource into a canonical key. Bare or `path:`
/// values that exist on disk are made absolute so two agents naming the
/// same file differently still collide.
pub(crate) fn resource_key(raw: &str) -> String {
    let value = match raw.split_once(':') {
        Some(("path", value)) => value,
        Some(_) => return raw.to_owned(),
        None => raw,
    };
    let path = PathBuf::from(value);
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&path))
            .unwrap_or(path)
    };
    format!("path:{}", hooks::normalize(&absolute).display())
}

fn parse_pairs(pairs: &[String]) -> Result<BTreeMap<String, String>> {
    pairs
        .iter()
        .map(|pair| {
            pair.split_once('=')
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .with_context(|| format!("expected KEY=VALUE, got `{pair}`"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_keys_are_absolute_paths_unless_typed() {
        assert_eq!(resource_key("task:ISSUE-1"), "task:ISSUE-1");
        assert_eq!(resource_key("branch:main"), "branch:main");
        let key = resource_key("does-not-exist-yet.rs");
        assert!(key.starts_with("path:/"), "{key}");
        assert!(key.ends_with("/does-not-exist-yet.rs"), "{key}");
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            resource_key(&format!("path:{}", cwd.display())),
            format!("path:{}", cwd.canonicalize().unwrap().display())
        );
    }
}
