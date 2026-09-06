//! `agentdocker`: command-line client for `agentd`.

mod agentfile;
mod client;
mod format;
mod hooks;
mod mcp;
mod service;
mod setup;
mod teams;

use std::collections::BTreeMap;
use std::path::PathBuf;

use agentdocker_core::{
    AgentRecord, AgentSpec, DiscoveredProcess, Lease, LeaseId, LeaseMode, MessageId, Request,
    Response, VcsState, protocol::DEFAULT_LEASE_TTL_SECS,
};
use agentdocker_core::{Change, ProjectRef};
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
    /// Build an image with an explicit engine and retain immutable input provenance.
    ImageBuild {
        #[arg(long)]
        /// Container engine: docker or podman (required; no fallback).
        engine: agentdocker_core::ContainerEngine,
        #[arg(long)]
        /// Docker context or Podman connection name.
        connection: Option<String>,
        /// Local directory to capture as build context (maximum 256 MiB).
        context: PathBuf,
        #[arg(short = 'f', long, default_value = "Containerfile")]
        /// Recipe path relative to the context.
        file: PathBuf,
        #[arg(long, default_value_t = 600)]
        /// Engine build timeout in seconds (1–3600).
        timeout: u64,
        #[arg(long)]
        /// Print the complete JSON record instead of only its ID.
        json: bool,
    },
    /// List retained image build records, including finished sessions.
    Images,
    /// Create a new linked checkout and branch without changing existing files.
    WorktreeCreate {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        /// Agent id, name or unique prefix (defaults to this session).
        agent: String,
        /// New checkout path, which becomes its physical identifier.
        path: String,
        #[arg(long)]
        /// Name of the new branch.
        branch: String,
    },
    /// Show tracked changes in this agent's checkout.
    WorktreeDiff {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        /// Agent id, name or unique prefix (defaults to this session).
        agent: String,
    },
    /// Preview or prepare an uncommitted merge of validated source code.
    Integrate {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        /// Agent id, name or unique prefix (defaults to this session).
        agent: String,
        /// Source worktree path containing the validated commit.
        source: String,
        #[arg(long)]
        /// Passing validation identifier for the source checkout.
        validation: String,
        #[arg(long)]
        /// Prepare an uncommitted merge and retain the target lease for review.
        apply: bool,
    },
    /// Issue a scoped container credential; the secret is written to a new private file.
    GrantAccess {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        /// Agent id, name or unique prefix (defaults to this session).
        agent: String,
        #[arg(long, default_value = "/workspace")]
        /// Checkout mount path inside the container.
        container_root: String,
        #[arg(long, default_value_t = 3600)]
        /// Credential lifetime in seconds (1–86400).
        ttl: u64,
        #[arg(long)]
        /// New private file in which to store the credential.
        token_file: PathBuf,
    },
    /// Revoke container access without prematurely releasing a live writer's leases.
    RevokeAccess { grant: String },
    /// Persist task context and content identity before an optional lease release.
    Checkpoint {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        #[arg(help = "Agent id, name or unique prefix (defaults to this session).")]
        agent: String,
        key: String,
        #[arg(long)]
        #[arg(help = "Task the replacement session should continue.")]
        task: String,
        #[arg(long = "assumption")]
        #[arg(help = "Assumption to review during recovery (repeatable).")]
        assumptions: Vec<String>,
        #[arg(long = "next")]
        #[arg(help = "Next action for the replacement (repeatable).")]
        next_steps: Vec<String>,
        #[arg(long)]
        #[arg(help = "Release this agent’s leases only after saving the checkpoint.")]
        release_leases: bool,
    },
    /// Inspect or explicitly accept a verified session handoff.
    Resume {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        #[arg(help = "Agent id, name or unique prefix (defaults to this session).")]
        agent: String,
        checkpoint: String,
        #[arg(long)]
        #[arg(help = "Accept this handoff after verifying unchanged content.")]
        acknowledge: bool,
    },
    /// List persisted checkpoints, including finished sessions.
    Checkpoints {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        #[arg(help = "Agent id, name or unique prefix (defaults to this session).")]
        agent: Option<String>,
    },
    /// Hand this agent's work to another: a checkpoint addressed to it, with leases, reads, changes, diff, unread messages and journal bundled around it.
    Handoff {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        /// Agent id, name or unique prefix (defaults to this session).
        agent: String,
        /// The recipient: id, name or unique prefix.
        to: String,
        #[arg(long)]
        /// What the recipient should continue.
        task: Option<String>,
        #[arg(long)]
        /// Anything the daemon does not already know.
        note: Option<String>,
        #[arg(long)]
        /// Move this agent's leases to the recipient when it accepts, instead of releasing them now.
        transfer_leases: bool,
        #[arg(long)]
        /// Retries with the same key return the same bundle.
        key: Option<String>,
    },
    /// List handoffs for --as or AGENTDOCKER_AGENT_ID; all when neither is set.
    Handoffs {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        /// Agent id, name or unique prefix.
        agent: Option<String>,
    },
    /// Write this agent's work as a bundle nobody is addressed to yet, as JSON on stdout, to carry to another host.
    Export {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        /// Agent id, name or unique prefix (defaults to this session).
        agent: String,
        #[arg(long)]
        /// What whoever imports it should continue.
        task: Option<String>,
        #[arg(long)]
        /// Anything the daemon does not already know.
        note: Option<String>,
    },
    /// Bring an exported bundle here for an agent, from a file or stdin; it is then accepted with `resume`.
    Import {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        /// Agent id, name or unique prefix (defaults to this session).
        agent: String,
        /// Bundle JSON file (default: stdin).
        file: Option<PathBuf>,
    },
    /// Execute a check and retain its command, log, and before/after code fingerprints.
    Validate {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        #[arg(help = "Agent id, name or unique prefix (defaults to this session).")]
        agent: String,
        #[arg(long, default_value_t = 300)]
        #[arg(help = "Maximum validation runtime in seconds (1–600).")]
        timeout: u64,
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<String>,
    },
    /// List retained validation evidence for a session.
    Validations {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        #[arg(help = "Agent id, name or unique prefix (defaults to this session).")]
        agent: String,
    },
    /// Record content immediately before reading files or searching a directory.
    Observe {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        #[arg(help = "Agent id, name or unique prefix (defaults to this session).")]
        agent: String,
        #[arg(required = true)]
        paths: Vec<String>,
    },
    /// Verify retained observations against current physical content.
    Stale {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        #[arg(help = "Agent id, name or unique prefix (defaults to this session).")]
        agent: String,
        paths: Vec<String>,
    },
    /// Show durable read observations for a session.
    Reads {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        #[arg(help = "Agent id, name or unique prefix (defaults to this session).")]
        agent: String,
    },
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
        /// Do not look for running agent processes nobody registered.
        #[arg(long)]
        no_discover: bool,
    },
    /// Find running agent processes (Claude Code, Codex, ...) nobody registered.
    Discover,
    /// The ledger: file changes seen in a project, with who held each file.
    Changes {
        /// Project: an id prefix or a path inside it (default: current directory).
        #[arg(long, value_name = "ID|PATH")]
        project: Option<String>,
        /// Only entries after this sequence number.
        #[arg(long)]
        since: Option<u64>,
        /// Only this file, or everything beneath a directory.
        #[arg(long)]
        path: Option<String>,
        /// Only changes attributed to this agent.
        #[arg(long)]
        agent: Option<String>,
        #[arg(short = 'n', long, default_value_t = 50)]
        limit: usize,
    },
    /// Paths changed in more than one checkout of a project: what will collide when the branches meet.
    Overlap {
        /// Project: an id prefix or a path inside it (default: current directory).
        #[arg(long, value_name = "ID|PATH")]
        project: Option<String>,
        /// Only ledger entries after this sequence number.
        #[arg(long)]
        since: Option<u64>,
        /// Only overlaps involving this agent's checkout.
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID", value_name = "AGENT")]
        agent: Option<String>,
    },
    /// Who changed a file, oldest first.
    Blame {
        path: String,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
    },
    /// Register a running process found by `discover`, by pid.
    Adopt {
        #[arg(required_unless_present = "all")]
        pid: Option<u32>,
        /// Register every process `discover` finds.
        #[arg(long, conflicts_with = "pid")]
        all: bool,
        /// Agent name (default: <runtime>-<pid>).
        #[arg(long, conflicts_with = "all")]
        name: Option<String>,
        /// Runtime (default: recognised from the command line, else custom).
        #[arg(long, conflicts_with = "all")]
        runtime: Option<String>,
    },
    /// The rooms agents share when they turn out to be on the same work: who is in them, and how the reviews stand.
    Channels {
        /// Project: an id prefix or a path inside it (default: the agent's own).
        #[arg(long, value_name = "ID|PATH")]
        project: Option<String>,
        /// Include closed channels that have not been pruned.
        #[arg(long)]
        all: bool,
        /// Only channels this agent is in.
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID", value_name = "AGENT")]
        agent: Option<String>,
    },
    /// Open, close, or prune a channel.
    Channel(ChannelArgs),
    /// Ask the other members of a channel to review this agent's work.
    ReviewRequest {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        /// Agent id, name or unique prefix (defaults to this session).
        agent: String,
        /// Channel id or unique prefix.
        channel: String,
        #[arg(long)]
        /// Anything the reviewers should know before they look.
        note: Option<String>,
    },
    /// Give a verdict on another member's work. Requested changes block it; approvals settle it.
    Review {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        /// Agent id, name or unique prefix (defaults to this session).
        agent: String,
        /// Channel id or unique prefix.
        channel: String,
        #[arg(long)]
        /// Whose work (default: the only other member).
        of: Option<String>,
        #[arg(long, group = "verdict")]
        /// Good to land.
        approve: bool,
        #[arg(long, group = "verdict")]
        /// Not yet; say what to change in the note.
        changes: bool,
        #[arg(long, group = "verdict")]
        /// Neither approves nor blocks.
        comment: bool,
        /// What you want to say.
        note: Option<String>,
    },
    /// The agent tools installed on this machine — CLI, version, apps — and whether AgentDocker is wired into each.
    Runtimes,
    /// Open the desktop app: a native window over the same socket, showing agents, runtimes, the journal, leases and events.
    Ui,
    /// Wire AgentDocker into the agent tools installed here: the MCP server registered with each runtime that takes one, hooks for Claude Code.
    Setup {
        /// Runtimes to set up (default: every installed one); see `runtimes`.
        runtimes: Vec<String>,
        /// Say what would change without changing it.
        #[arg(long)]
        dry_run: bool,
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
    /// Replace a managed container after confirming exit; print the new agent ID.
    Restart { agent: String },
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
        /// What changed and why; becomes the project's journal entry.
        #[arg(long)]
        summary: Option<String>,
    },
    /// The project's journal: what changed and why, one line per release, note, or commit.
    Journal(JournalArgs),
    /// List leases.
    Leases {
        /// Only leases held by this agent.
        #[arg(long)]
        agent: Option<String>,
        /// Only leases overlapping this resource.
        #[arg(long)]
        resource: Option<String>,
    },
    /// Run agentd as a login service, or start, stop, and inspect it.
    Daemon(service::DaemonArgs),
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
    /// Run inside this recorded image build (mounts and network are opt-in).
    #[arg(long)]
    image_build: Option<String>,
    /// Mount the checkout and a private authenticated coordination endpoint.
    #[arg(long, requires = "image_build")]
    mount_checkout: bool,
    /// Running rootless Podman machine for macOS checkout/socket transport.
    #[arg(long, requires = "mount_checkout")]
    podman_machine: Option<String>,
    /// Use an engine-volume coordination socket (automatic on Docker Desktop).
    #[arg(long, requires = "mount_checkout")]
    engine_relay: bool,
    /// Container networking (none or bridge); host networking is unavailable.
    #[arg(long, requires = "image_build", value_parser = ["none", "bridge"])]
    network: Option<String>,
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
    /// Give the agent its own linked worktree and branch (agent/<name>) under the daemon home, so its edits are a layer of their own.
    #[arg(long)]
    isolate: bool,
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
struct ChannelArgs {
    #[command(subcommand)]
    action: ChannelAction,
}

#[derive(Subcommand)]
enum ChannelAction {
    /// Open a channel for a task, rather than wait for a collision.
    Open {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        /// Agent id, name or unique prefix (defaults to this session).
        agent: String,
        /// What the channel is about.
        task: String,
        #[arg(long = "with", value_name = "AGENT")]
        /// Who to put in it (default: everyone else in the project).
        members: Vec<String>,
    },
    /// The work is final: close it and tell the members.
    Close {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        /// Agent id, name or unique prefix (defaults to this session).
        agent: String,
        /// Channel id or unique prefix.
        channel: String,
        #[arg(long)]
        /// What the work settled on.
        resolution: Option<String>,
    },
    /// Forget channels closed longer ago than this.
    Prune {
        /// Project: an id prefix or a path inside it (default: every project).
        #[arg(long, value_name = "ID|PATH")]
        project: Option<String>,
        #[arg(long, default_value_t = 14 * 24 * 60 * 60, value_name = "SECONDS")]
        /// How long a closed channel is kept.
        before: u64,
    },
}

#[derive(Args)]
struct JournalArgs {
    #[command(subcommand)]
    action: Option<JournalAction>,
    /// Project: an id prefix or a path inside it (default: current directory).
    #[arg(long, value_name = "ID|PATH")]
    project: Option<String>,
    /// Only entries after this sequence number.
    #[arg(long)]
    since: Option<u64>,
    /// Only entries up to this sequence number.
    #[arg(long)]
    until: Option<u64>,
    /// Only entries by this agent.
    #[arg(long)]
    agent: Option<String>,
    /// Only entries made on this branch.
    #[arg(long)]
    branch: Option<String>,
    /// release, note, commit, join, leave, or handoff.
    #[arg(long)]
    kind: Option<String>,
    /// Only entries touching this file, or anything beneath a directory.
    #[arg(long)]
    path: Option<String>,
    /// Full-text search over summaries.
    #[arg(long)]
    grep: Option<String>,
    /// How many of the newest matching entries to show.
    #[arg(short = 'n', long, default_value_t = 50)]
    limit: usize,
    /// Keep printing entries as they are appended.
    #[arg(long)]
    follow: bool,
    /// Only what the reader has not been shown yet: the human's cursor,
    /// or an agent's with --as.
    #[arg(long, conflicts_with = "follow")]
    new: bool,
    /// With --new: mark what was shown as seen.
    #[arg(long, requires = "new")]
    ack: bool,
    /// With --new: show entries from every branch instead of counting them.
    #[arg(long, requires = "new")]
    all_branches: bool,
    /// With --new: reader identity (defaults to AGENTDOCKER_AGENT_ID, then user).
    #[arg(long = "as", value_name = "AGENT", requires = "new")]
    reader: Option<String>,
}

#[derive(Subcommand)]
enum JournalAction {
    /// Append a note to the journal of the agent's project.
    Add {
        #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
        agent: String,
        summary: String,
    },
    /// Drop entries below a sequence number.
    Prune {
        /// Delete every entry whose sequence number is below this.
        #[arg(long)]
        before: u64,
        /// Project: an id prefix or a path inside it (default: current directory).
        #[arg(long, value_name = "ID|PATH")]
        project: Option<String>,
    },
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
    let socket = cli.socket.clone();
    let client = Client::new(cli.socket);

    match cli.command {
        Command::ImageBuild {
            engine,
            connection,
            context,
            file,
            timeout,
            json,
        } => {
            let context = std::env::current_dir()?.join(context);
            let response = client
                .call(&Request::BuildImage {
                    spec: agentdocker_core::ImageBuildSpec {
                        engine,
                        connection,
                        context,
                        recipe: file,
                        timeout_secs: timeout,
                    },
                })
                .await?;
            match response {
                Response::ImageBuild { build } if !json => println!("{}", build.id),
                response @ Response::ImageBuild { .. } => print_json(&response)?,
                _ => bail!("unexpected image build response"),
            }
        }
        Command::Images => print_json(&client.call(&Request::Images).await?)?,
        Command::Observe { agent, paths } => {
            print_json(&client.call(&Request::Observe { agent, paths }).await?)?;
        }
        Command::Stale { agent, paths } => {
            let response = client.call(&Request::Stale { agent, paths }).await?;
            print_json(&response)?;
            if matches!(response, Response::Stale { stale } if !stale.is_empty()) {
                bail!("context is stale; reread the reported paths");
            }
        }
        Command::Reads { agent } => {
            print_json(&client.call(&Request::Reads { agent }).await?)?;
        }
        Command::Checkpoint {
            agent,
            key,
            task,
            assumptions,
            next_steps,
            release_leases,
        } => {
            let response = client
                .call(&Request::Checkpoint {
                    agent,
                    key,
                    task,
                    assumptions,
                    next_steps,
                    release_leases,
                })
                .await?;
            if let Response::Checkpoint { checkpoint } = response {
                println!("{}", checkpoint.id);
            } else {
                bail!("unexpected checkpoint response");
            }
        }

        Command::Resume {
            agent,
            checkpoint,
            acknowledge,
        } => print_json(
            &client
                .call(&Request::Resume {
                    agent,
                    checkpoint,
                    acknowledge,
                })
                .await?,
        )?,
        Command::Checkpoints { agent } => {
            print_json(&client.call(&Request::Checkpoints { agent }).await?)?
        }
        Command::Handoff {
            agent,
            to,
            task,
            note,
            transfer_leases,
            key,
        } => {
            let request = Request::Handoff {
                agent,
                to: Some(to),
                task,
                note,
                transfer_leases,
                key,
            };
            match client.call(&request).await? {
                Response::Handoff { bundle } => println!("{}", bundle.id),
                other => bail!("unexpected reply to handoff: {other:?}"),
            }
        }
        Command::Handoffs { agent } => {
            print_json(&client.call(&Request::Handoffs { agent }).await?)?
        }
        Command::Export { agent, task, note } => {
            let request = Request::Handoff {
                agent,
                to: None,
                task,
                note,
                transfer_leases: false,
                key: None,
            };
            match client.call(&request).await? {
                Response::Handoff { bundle } => print_json(&bundle)?,
                other => bail!("unexpected reply to export: {other:?}"),
            }
        }
        Command::Import { agent, file } => {
            let raw = match &file {
                Some(path) => read_import(
                    std::fs::File::open(path)
                        .with_context(|| format!("cannot read {}", path.display()))?,
                )?,
                None => read_import(std::io::stdin())?,
            };
            let bundle: agentdocker_core::HandoffBundle =
                serde_json::from_str(&raw).context("not a handoff bundle")?;
            let request = Request::Import {
                agent,
                bundle: Box::new(bundle),
            };
            match client.call(&request).await? {
                Response::Handoff { bundle } => println!("{}", bundle.id),
                other => bail!("unexpected reply to import: {other:?}"),
            }
        }
        Command::Validations { agent } => {
            print_json(&client.call(&Request::Validations { agent }).await?)?
        }
        Command::Validate {
            agent,
            command,
            timeout,
        } => {
            let response = client
                .call(&Request::Validate {
                    agent,
                    command,
                    timeout_secs: timeout,
                })
                .await?;
            if let Response::Validation { validation, .. } = &response {
                println!("{}", validation.id);
            }
            if !matches!(response, Response::Validation { passed: true, .. }) {
                bail!(
                    "validation did not pass for unchanged content; inspect its log and evidence"
                );
            }
        }
        Command::WorktreeCreate {
            agent,
            path,
            branch,
        } => {
            match client
                .call(&Request::WorktreeCreate {
                    agent,
                    path,
                    branch,
                })
                .await?
            {
                Response::Worktree { path, .. } => println!("{}", path.display()),
                _ => bail!("unexpected worktree response"),
            }
        }
        Command::WorktreeDiff { agent } => {
            print_json(&client.call(&Request::WorktreeDiff { agent }).await?)?
        }
        Command::Integrate {
            agent,
            source,
            validation,
            apply,
        } => {
            let response = client
                .call(&Request::Integrate {
                    agent,
                    source,
                    validation,
                    apply,
                })
                .await?;
            print_json(&response)?;
            if matches!(
                response,
                Response::Integration {
                    applied: true,
                    clean: false,
                    ..
                }
            ) {
                bail!(
                    "merge has conflicts; integration lease retained, inspect and resolve or abort with Git"
                );
            }
        }
        Command::RevokeAccess { grant } => {
            print_json(&client.call(&Request::RevokeAccess { grant }).await?)?
        }
        Command::GrantAccess {
            agent,
            container_root,
            ttl,
            token_file,
        } => {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            // Reserve the private output before creating a credential.
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&token_file)?;
            let response = client
                .call(&Request::GrantAccess {
                    agent,
                    container_root,
                    ttl_secs: ttl,
                })
                .await?;
            if let Response::Access {
                grant,
                token,
                socket: _,
                expires_at: _,
            } = response
            {
                if let Err(error) = writeln!(file, "{token}").and_then(|_| file.sync_all()) {
                    let _ = client.call(&Request::RevokeAccess { grant }).await;
                    return Err(error.into());
                }
                println!("{grant}");
            } else {
                bail!("unexpected access response");
            }
        }
        Command::Ping => {
            if let Response::Pong {
                version,
                uptime_secs,
                ..
            } = client.call(&Request::Ping).await?
            {
                println!("agentd {version} up {}", format::span_secs(uptime_secs));
            }
        }
        Command::Ps {
            all,
            project,
            labels,
            no_discover,
        } => {
            let request = Request::List {
                all,
                project: project.as_deref().map(project_selector),
                labels: parse_pairs(&labels)?,
            };
            let Response::Agents { agents } = client.call(&request).await? else {
                return Ok(());
            };
            let mut unadopted = Vec::new();
            if !no_discover && project.is_none() && labels.is_empty() {
                if let Response::Processes { processes } = client.call(&Request::Discover).await? {
                    unadopted = processes;
                }
            }
            print_agents(&agents, &unadopted);
            if !unadopted.is_empty() {
                eprintln!(
                    "{} running agent process(es) nobody registered; `agentdocker adopt <pid>` brings one in",
                    unadopted.len()
                );
            }
        }
        Command::Changes {
            project,
            since,
            path,
            agent,
            limit,
        } => {
            let request = Request::Changes {
                project: project_selector(project.as_deref().unwrap_or(".")),
                since_seq: since,
                path: path.as_deref().map(absolute_path),
                agent,
                limit,
            };
            if let Response::Changes { changes } = client.call(&request).await? {
                print_changes(&client, &changes).await?;
            }
        }
        Command::Overlap {
            project,
            since,
            agent,
        } => {
            let request = Request::Overlap {
                project: project_selector(project.as_deref().unwrap_or(".")),
                since_seq: since,
                agent,
            };
            if let Response::Overlap { overlaps } = client.call(&request).await? {
                print_overlaps(&client, &overlaps).await?;
            }
        }
        Command::Blame { path, limit } => {
            let absolute = absolute_path(&path);
            let project = PathBuf::from(&absolute)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| absolute.clone());
            let request = Request::Changes {
                project,
                since_seq: None,
                path: Some(absolute),
                agent: None,
                limit,
            };
            if let Response::Changes { changes } = client.call(&request).await? {
                print_changes(&client, &changes).await?;
            }
        }
        Command::Discover => {
            if let Response::Processes { processes } = client.call(&Request::Discover).await? {
                print_processes(&processes);
            }
        }
        Command::Adopt {
            pid,
            all,
            name,
            runtime,
        } => {
            if all {
                let Response::Processes { processes } = client.call(&Request::Discover).await?
                else {
                    bail!("unexpected reply to discover");
                };
                let mut failed = 0;
                for process in processes {
                    match client
                        .call_raw(&Request::Adopt {
                            pid: process.pid,
                            name: None,
                            runtime: None,
                        })
                        .await?
                    {
                        Response::Agent { agent } => println!("{}", agent.id),
                        Response::Error { message, .. } => {
                            failed += 1;
                            eprintln!("pid {}: {message}", process.pid);
                        }
                        _ => {}
                    }
                }
                if failed > 0 {
                    bail!("{failed} process(es) could not be adopted");
                }
            } else if let Some(pid) = pid {
                let request = Request::Adopt { pid, name, runtime };
                if let Response::Agent { agent } = client.call(&request).await? {
                    println!("{}", agent.id);
                }
            }
        }
        Command::Channels {
            project,
            all,
            agent,
        } => {
            let request = Request::Channels {
                project: project.as_deref().map(project_selector).unwrap_or_default(),
                all,
                agent,
            };
            if let Response::Channels { channels } = client.call(&request).await? {
                print_channels(&client, &channels).await?;
            }
        }
        Command::Channel(args) => match args.action {
            ChannelAction::Open {
                agent,
                task,
                members,
            } => {
                let request = Request::ChannelOpen {
                    agent,
                    task,
                    members,
                };
                if let Response::Channel { channel } = client.call(&request).await? {
                    println!("{}", channel.id);
                }
            }
            ChannelAction::Close {
                agent,
                channel,
                resolution,
            } => {
                let request = Request::ChannelClose {
                    agent,
                    channel,
                    resolution,
                };
                if let Response::Channel { channel } = client.call(&request).await? {
                    eprintln!("closed {}", channel.id);
                }
            }
            ChannelAction::Prune { project, before } => {
                let request = Request::ChannelPrune {
                    project: project.as_deref().map(project_selector).unwrap_or_default(),
                    before_secs: before,
                };
                if let Response::Pruned { removed } = client.call(&request).await? {
                    eprintln!("forgot {removed} closed channel(s)");
                }
            }
        },
        Command::ReviewRequest {
            agent,
            channel,
            note,
        } => {
            let request = Request::ReviewRequest {
                agent,
                channel,
                note,
            };
            if let Response::Channel { channel } = client.call(&request).await? {
                eprintln!("asked {} member(s) to review", channel.members.len() - 1);
            }
        }
        Command::Review {
            agent,
            channel,
            of,
            approve,
            changes,
            comment: _,
            note,
        } => {
            let verdict = if approve {
                "approve"
            } else if changes {
                "changes"
            } else {
                "comment"
            };
            let request = Request::Review {
                agent: agent.clone(),
                channel,
                of,
                verdict: verdict.to_owned(),
                note,
            };
            if let Response::Channel { channel } = client.call(&request).await? {
                print_reviews(&client, &channel).await?;
            }
        }
        Command::Runtimes => {
            if let Response::Runtimes { runtimes } = client.call(&Request::Runtimes).await? {
                print_runtimes(&runtimes);
            }
        }
        Command::Ui => {
            let app = std::env::current_exe()
                .ok()
                .and_then(|me| me.parent().map(|dir| dir.join("agentdocker-ui")))
                .filter(|sibling| sibling.is_file())
                .or_else(|| {
                    std::env::var_os("PATH").and_then(|path| {
                        std::env::split_paths(&path)
                            .map(|dir| dir.join("agentdocker-ui"))
                            .find(|candidate| candidate.is_file())
                    })
                })
                .context(
                    "agentdocker-ui is not installed beside agentdocker or on PATH; build it with `cargo install --path crates/ui --locked`",
                )?;
            let mut child = std::process::Command::new(&app);
            // The app reads AGENTDOCKER_SOCKET; pass on whatever this
            // invocation was pointed at so both talk to one daemon.
            if let Some(socket) = &socket {
                child.env("AGENTDOCKER_SOCKET", socket);
            }
            child
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .with_context(|| format!("cannot start {}", app.display()))?;
        }
        Command::Setup { runtimes, dry_run } => setup::run(&client, &runtimes, dry_run).await?,
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
                isolate: args.isolate,
            };
            let request = match args.image_build {
                Some(build) => Request::RunContainer {
                    spec,
                    build,
                    options: agentdocker_core::container::ContainerRunOptions {
                        engine_relay: args.engine_relay,
                        mount_checkout: args.mount_checkout,
                        podman_machine: args.podman_machine,
                        network: match args.network.as_deref().unwrap_or("none") {
                            "bridge" => agentdocker_core::container::ContainerNetwork::Bridge,
                            _ => agentdocker_core::container::ContainerNetwork::None,
                        },
                    },
                },
                None => Request::Run { spec },
            };
            if let Response::Agent { agent } = client.call(&request).await? {
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
                isolate: false,
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
        Command::Restart { agent } => {
            if let Response::Agent { agent } =
                client.call(&Request::RestartContainer { agent }).await?
            {
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
        Command::Release {
            agent,
            lease,
            all,
            summary,
        } => {
            if all {
                let request = Request::ReleaseAll {
                    agent,
                    summary,
                    summary_source: agentdocker_core::SummarySource::Explicit,
                };
                if let Response::Leases { leases } = client.call(&request).await? {
                    for lease in leases {
                        println!("{}", lease.id);
                    }
                }
            } else if let Some(lease) = lease {
                let request = Request::Release {
                    agent,
                    lease: LeaseId::from(lease.as_str()),
                    summary,
                    summary_source: agentdocker_core::SummarySource::Explicit,
                };
                client.call(&request).await?;
            }
        }
        Command::Journal(args) => journal_command(&client, args).await?,
        Command::Leases { agent, resource } => {
            let request = Request::Leases {
                agent,
                resource: resource.as_deref().map(resource_key),
            };
            if let Response::Leases { leases } = client.call(&request).await? {
                print_leases(&leases);
            }
        }
        Command::Daemon(args) => service::run(socket, args).await?,
        Command::Hook(args) => hooks::run(client, args).await?,
        Command::Mcp(args) => mcp::serve(client, args).await?,
        Command::Up { file, names } => teams::up(&client, file.as_deref(), &names).await?,
        Command::Down { file, names, force } => {
            teams::down(&client, file.as_deref(), &names, force).await?;
        }
        Command::Events { replay } => {
            client
                .stream(
                    &Request::Events {
                        replay,
                        ready: false,
                    },
                    |response| {
                        if let Response::Event { event } = response {
                            println!("{}", format::event_line(&event));
                        }
                        Ok(true)
                    },
                )
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

async fn journal_command(client: &Client, args: JournalArgs) -> Result<()> {
    match args.action {
        Some(JournalAction::Add { agent, summary }) => {
            if let Response::JournalEntry { entry } =
                client.call(&Request::JournalAdd { agent, summary }).await?
            {
                println!("{}", entry.seq);
            }
        }
        Some(JournalAction::Prune { before, project }) => {
            let request = Request::JournalPrune {
                project: project_selector(project.as_deref().unwrap_or(".")),
                before_seq: before,
            };
            if let Response::Pruned { removed } = client.call(&request).await? {
                println!("removed {removed} entries");
            }
        }
        None if args.new => {
            let request = Request::Journal {
                project: project_selector(args.project.as_deref().unwrap_or(".")),
                since_seq: args.since,
                until_seq: None,
                agent: None,
                branch: None,
                kind: None,
                path: None,
                grep: None,
                limit: args.limit,
                digest: Some(agentdocker_core::DigestRequest {
                    reader: args
                        .reader
                        .or_else(|| std::env::var("AGENTDOCKER_AGENT_ID").ok())
                        .unwrap_or_else(|| "user".to_owned()),
                    max_entries: args.limit,
                    max_chars: 100_000,
                    all_branches: args.all_branches,
                    advance: args.ack,
                }),
            };
            if let Response::Digest { digest, .. } = client.call(&request).await? {
                if digest.text.is_empty() {
                    println!("Nothing new.");
                } else {
                    print!("{}", digest.text);
                }
            }
        }
        None => {
            let path = args.path.as_deref().map(absolute_path);
            let request = Request::Journal {
                project: project_selector(args.project.as_deref().unwrap_or(".")),
                since_seq: args.since,
                until_seq: args.until,
                agent: args.agent.clone(),
                branch: args.branch.clone(),
                kind: args.kind.clone(),
                path: path.clone(),
                grep: args.grep.clone(),
                limit: args.limit,
                digest: None,
            };
            let print = |entry: &agentdocker_core::JournalEntry| {
                println!(
                    "{:>6}  {}  {}",
                    entry.seq,
                    format::clock(entry.at),
                    entry.line()
                );
            };
            // The snapshot, and where its tail starts.
            let snapshot = async {
                let Response::Journal { project, entries } = client.call(&request).await? else {
                    return Ok(None);
                };
                entries.iter().for_each(print);
                let last = entries.last().map_or(args.since.unwrap_or(0), |e| e.seq);
                Ok(Some((project, last)))
            };
            if !args.follow {
                snapshot.await?;
                return Ok(());
            }
            // Wait for subscription readiness before taking the snapshot.
            // Its sequence bound drops entries already printed from that snapshot.
            let filter = agentdocker_core::JournalFilter {
                agent: args.agent,
                branch: args.branch,
                kind: args
                    .kind
                    .as_deref()
                    .and_then(agentdocker_core::JournalKind::parse),
                path: path.map(PathBuf::from),
                grep: args.grep,
                until_seq: args.until,
            };
            client
                .stream_after(
                    &Request::Events {
                        replay: 0,
                        ready: true,
                    },
                    snapshot,
                    |seen, response| {
                        let Some((project, last)) = seen else {
                            return Ok(false);
                        };
                        if let Response::Event { event } = response {
                            if let agentdocker_core::EventKind::JournalAppended { entry } =
                                event.kind
                            {
                                if entry.project == *project
                                    && entry.seq > *last
                                    && filter.matches(&entry)
                                {
                                    print(&entry);
                                }
                            }
                        }
                        Ok(true)
                    },
                )
                .await?;
        }
    }
    Ok(())
}

/// Absolute, with as much of it canonical as exists.
fn absolute_path(raw: &str) -> String {
    let path = PathBuf::from(raw);
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&path))
            .unwrap_or(path)
    };
    agentdocker_host::project::canonical(&absolute)
        .to_string_lossy()
        .into_owned()
}

/// Ledger rows, with agent ids turned into names.
async fn print_changes(client: &Client, changes: &[Change]) -> Result<()> {
    let names: BTreeMap<String, String> = match client
        .call(&Request::List {
            all: true,
            project: None,
            labels: BTreeMap::new(),
        })
        .await
    {
        Ok(Response::Agents { agents }) => agents
            .into_iter()
            .map(|a| (a.id.to_string(), a.spec.name))
            .collect(),
        _ => BTreeMap::new(),
    };
    let rows: Vec<Vec<String>> = changes
        .iter()
        .map(|c| {
            let (by, note) = match &c.by {
                agentdocker_core::Attribution::Agent { agent, note, .. } => (
                    names
                        .get(agent.as_str())
                        .cloned()
                        .unwrap_or_else(|| agent.short().to_owned()),
                    note.clone().unwrap_or_default(),
                ),
                agentdocker_core::Attribution::External => ("external".to_owned(), String::new()),
            };
            vec![
                c.seq.to_string(),
                format::clock(c.at),
                c.kind.to_string(),
                c.path.display().to_string(),
                by,
                c.head
                    .as_deref()
                    .map(|h| h.chars().take(7).collect())
                    .unwrap_or_else(|| "-".to_owned()),
                note,
            ]
        })
        .collect();
    format::table(
        &["SEQ", "WHEN", "KIND", "PATH", "BY", "HEAD", "NOTE"],
        &rows,
    );
    Ok(())
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

/// Agents, then — dimmed — running agent processes nobody registered,
/// shown under the name `adopt` would give them.
fn print_agents(agents: &[AgentRecord], unadopted: &[DiscoveredProcess]) {
    let mut rows: Vec<Vec<String>> = agents
        .iter()
        .map(|a| {
            vec![
                a.id.short().to_owned(),
                a.spec.name.clone(),
                project_cell(a),
                a.vcs
                    .as_ref()
                    .map(|v| match (&v.branch, &v.head) {
                        (Some(branch), _) => branch.clone(),
                        (None, Some(_)) => "(detached)".to_owned(),
                        (None, None) => "-".to_owned(),
                    })
                    .unwrap_or_else(|| "-".to_owned()),
                a.vcs
                    .as_ref()
                    .and_then(VcsState::short_head)
                    .unwrap_or("-")
                    .to_owned(),
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
    let first_unadopted = rows.len();
    rows.extend(unadopted.iter().map(|p| {
        vec![
            "-".to_owned(),
            p.default_name(),
            p.project
                .as_ref()
                .map(ProjectRef::name)
                .unwrap_or_else(|| "-".to_owned()),
            "-".to_owned(),
            "-".to_owned(),
            p.runtime.clone(),
            "-".to_owned(),
            "unadopted".to_owned(),
            p.pid.to_string(),
            p.started_at
                .map(format::ago)
                .unwrap_or_else(|| "-".to_owned()),
        ]
    }));
    format::table_dimming(
        &[
            "AGENT ID", "NAME", "PROJECT", "BRANCH", "HEAD", "RUNTIME", "MODEL", "STATUS", "PID",
            "CREATED",
        ],
        &rows,
        |i| i >= first_unadopted,
    );
}

/// One row per channel, with how many reviews it carries.
async fn print_channels(client: &Client, channels: &[agentdocker_core::Channel]) -> Result<()> {
    if channels.is_empty() {
        println!("no channels; the daemon opens one when two checkouts change the same path");
        return Ok(());
    }
    let names = agent_names(client).await;
    let rows: Vec<Vec<String>> = channels
        .iter()
        .map(|c| {
            vec![
                c.id.to_string(),
                if c.is_open() { "open" } else { "closed" }.to_owned(),
                c.title(),
                c.members
                    .iter()
                    .map(|m| named(&names, m))
                    .collect::<Vec<_>>()
                    .join(", "),
                c.reviews.len().to_string(),
                format::ago(c.opened_at),
            ]
        })
        .collect();
    format::table(
        &["CHANNEL", "STATE", "ABOUT", "MEMBERS", "REVIEWS", "OPENED"],
        &rows,
    );
    Ok(())
}

/// Where every member's work stands after a review.
async fn print_reviews(client: &Client, channel: &agentdocker_core::Channel) -> Result<()> {
    let names = agent_names(client).await;
    for review in &channel.reviews {
        println!("{}  {}", format::clock(review.at), review.line());
    }
    for member in &channel.members {
        match channel.decision(member, 1) {
            agentdocker_core::Decision::Approved { approvals } => {
                println!("{}: approved ({approvals})", named(&names, member));
            }
            agentdocker_core::Decision::Blocked { by } => println!(
                "{}: blocked by {}",
                named(&names, member),
                by.iter()
                    .map(|b| named(&names, b))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            agentdocker_core::Decision::Pending { approvals, needed } => {
                if approvals > 0 || channel.reviews.iter().any(|r| &r.of == member) {
                    println!(
                        "{}: {approvals} of {needed} approvals",
                        named(&names, member)
                    );
                }
            }
        }
    }
    Ok(())
}

/// Agent names by id, for rendering; ids stand in when the daemon cannot say.
async fn agent_names(client: &Client) -> BTreeMap<String, String> {
    match client
        .call(&Request::List {
            all: true,
            project: None,
            labels: BTreeMap::new(),
        })
        .await
    {
        Ok(Response::Agents { agents }) => agents
            .into_iter()
            .map(|a| (a.id.to_string(), a.spec.name))
            .collect(),
        _ => BTreeMap::new(),
    }
}

fn named(names: &BTreeMap<String, String>, id: &agentdocker_core::AgentId) -> String {
    names
        .get(id.as_str())
        .cloned()
        .unwrap_or_else(|| id.short().to_owned())
}

/// One row per known runtime; installed ones first.
fn print_runtimes(runtimes: &[agentdocker_core::RuntimeInfo]) {
    let mut sorted: Vec<&agentdocker_core::RuntimeInfo> = runtimes.iter().collect();
    sorted.sort_by_key(|r| !r.installed());
    let rows: Vec<Vec<String>> = sorted
        .iter()
        .map(|r| {
            let apps = r
                .apps
                .iter()
                .map(|a| match &a.version {
                    Some(v) => format!("{} {v}", a.label),
                    None => a.label.clone(),
                })
                .collect::<Vec<_>>()
                .join(", ");
            vec![
                r.name.clone(),
                r.vendor.clone(),
                r.cli
                    .as_ref()
                    .map(|c| c.display().to_string())
                    .unwrap_or_else(|| "-".to_owned()),
                r.version.clone().unwrap_or_else(|| "-".to_owned()),
                if apps.is_empty() {
                    "-".to_owned()
                } else {
                    apps
                },
                r.mcp.symbol().to_owned(),
                r.hooks.symbol().to_owned(),
                r.running.to_string(),
            ]
        })
        .collect();
    format::table(
        &[
            "RUNTIME", "VENDOR", "CLI", "VERSION", "APP", "MCP", "HOOKS", "RUNNING",
        ],
        &rows,
    );
    if runtimes.iter().any(|r| {
        r.installed()
            && (r.mcp == agentdocker_core::Wiring::Missing
                || r.hooks == agentdocker_core::Wiring::Missing)
    }) {
        println!(
            "\n`agentdocker setup` wires AgentDocker into what is installed; `--dry-run` shows what it would change."
        );
    }
}

fn print_processes(processes: &[DiscoveredProcess]) {
    let rows: Vec<Vec<String>> = processes
        .iter()
        .map(|p| {
            let mut command: String = p.command.chars().take(60).collect();
            if p.command.chars().count() > 60 {
                command.push('…');
            }
            vec![
                p.pid.to_string(),
                p.runtime.clone(),
                p.project
                    .as_ref()
                    .map(ProjectRef::name)
                    .unwrap_or_else(|| "-".to_owned()),
                p.cwd
                    .as_ref()
                    .map(|c| c.display().to_string())
                    .unwrap_or_else(|| "-".to_owned()),
                p.started_at
                    .map(format::ago)
                    .unwrap_or_else(|| "-".to_owned()),
                command,
            ]
        })
        .collect();
    format::table(
        &["PID", "RUNTIME", "PROJECT", "CWD", "STARTED", "COMMAND"],
        &rows,
    );
}

fn print_leases(leases: &[Lease]) {
    let rows: Vec<Vec<String>> = leases
        .iter()
        .map(|l| {
            vec![
                l.id.to_string(),
                format::resource(&l.resource),
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

/// One block per path: the path, then each checkout that changed it with
/// who did it there, how often, and how recently.
async fn print_overlaps(client: &Client, overlaps: &[agentdocker_core::Overlap]) -> Result<()> {
    if overlaps.is_empty() {
        println!("no path was changed in more than one checkout");
        return Ok(());
    }
    let names: BTreeMap<String, String> = match client
        .call(&Request::List {
            all: true,
            project: None,
            labels: BTreeMap::new(),
        })
        .await
    {
        Ok(Response::Agents { agents }) => agents
            .into_iter()
            .map(|a| (a.id.to_string(), a.spec.name))
            .collect(),
        _ => BTreeMap::new(),
    };
    let now = chrono::Utc::now();
    for overlap in overlaps {
        println!("{}", overlap.path.display());
        for party in &overlap.parties {
            let who = if party.agents.is_empty() {
                "external".to_owned()
            } else {
                party
                    .agents
                    .iter()
                    .map(|a| {
                        names
                            .get(a.as_str())
                            .cloned()
                            .unwrap_or_else(|| a.short().to_owned())
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let head = party
                .head
                .as_deref()
                .map(|h| format!("@{}", h.chars().take(7).collect::<String>()))
                .unwrap_or_default();
            let kind = if party.worktree.is_some() {
                "worktree"
            } else {
                "checkout"
            };
            println!(
                "    {} {}{}  {who} ×{}  {}",
                kind,
                party.checkout.display(),
                head,
                party.changes,
                agentdocker_core::journal::ago(now, party.last_at)
            );
        }
    }
    Ok(())
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn read_import(reader: impl std::io::Read) -> Result<String> {
    use std::io::Read;
    let limit = agentdocker_core::handoff::IMPORT_BYTES;
    let mut raw = String::new();
    reader.take((limit + 1) as u64).read_to_string(&mut raw)?;
    if raw.len() > limit {
        bail!("handoff import exceeds the 8 MiB serialized limit");
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_stops_reading_at_the_shared_size_limit() {
        let input = std::io::repeat(b'x');
        assert!(
            read_import(input)
                .unwrap_err()
                .to_string()
                .contains("8 MiB")
        );
        assert_eq!(
            read_import(&b"{\"schema\":2}"[..]).unwrap(),
            "{\"schema\":2}"
        );
    }

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
