//! `agentdocker`: command-line client for `agentd`.

mod client;
mod format;

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
    /// List agents (live ones by default).
    Ps {
        /// Include finished agents.
        #[arg(short, long)]
        all: bool,
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
    /// Send a message to an agent, a topic (`topic:name`), or everyone (`all`).
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
    /// Release a lease you hold.
    Release {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        agent: String,
        lease: String,
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
    /// Agent id/name, `topic:<name>`, or `all`.
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
    /// `kind:value`. A bare path that exists becomes `path:<absolute>`.
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
        Command::Ps { all } => {
            if let Response::Agents { agents } = client.call(&Request::List { all }).await? {
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
            let spec = AgentSpec {
                name: args.name,
                runtime: args.runtime,
                provider: args.provider,
                model: args.model,
                command: Vec::new(),
                workdir: args.workdir.map(|d| d.canonicalize().unwrap_or(d)),
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
                to: args.to,
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
        Command::Release { agent, lease } => {
            let request = Request::Release {
                agent,
                lease: LeaseId::from(lease.as_str()),
            };
            client.call(&request).await?;
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

fn print_agents(agents: &[AgentRecord]) {
    let rows: Vec<Vec<String>> = agents
        .iter()
        .map(|a| {
            vec![
                a.id.short().to_owned(),
                a.spec.name.clone(),
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
            "AGENT ID", "NAME", "RUNTIME", "MODEL", "STATUS", "PID", "CREATED",
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
fn resource_key(raw: &str) -> String {
    let value = match raw.split_once(':') {
        Some(("path", value)) => value,
        Some(_) => return raw.to_owned(),
        None => raw,
    };
    match PathBuf::from(value).canonicalize() {
        Ok(absolute) => format!("path:{}", absolute.display()),
        Err(_) => raw.to_owned(),
    }
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
