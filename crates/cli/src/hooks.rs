//! Claude Code hooks adapter.
//!
//! `agentdocker hook claude-code` is wired into Claude Code's hook events by
//! `agentdocker hook install claude-code`. Each invocation reads one event
//! as JSON on stdin and turns it into daemon calls:
//!
//! | event              | effect                                                                 |
//! |--------------------|------------------------------------------------------------------------|
//! | `SessionStart`     | register the session as an agent; tell the model who else is running, hand it queued messages and the project journal since it last looked |
//! | `UserPromptSubmit` | hand the model queued messages and new journal entries as context      |
//! | `PreToolUse`       | claim `path:<file>` before Edit/Write/MultiEdit/NotebookEdit; deny the edit on conflict |
//! | `PostToolUse`      | hand the model queued messages as context                              |
//! | `Stop`             | release every lease with the transcript's last message as the journal summary; block once when messages wait |
//! | `SessionEnd`       | release every lease and deregister                                     |
//!
//! The hook never breaks a session: if agentd is unreachable it prints a
//! note to stderr and exits 0, and Claude Code carries on as if the hook
//! were not there (an edit is allowed rather than denied).

use std::cell::RefCell;
use std::io::Read;
use std::os::unix::process::parent_id;
use std::path::{Path, PathBuf};

use agentdocker_core::journal::transcript_summary;
use agentdocker_core::{
    AgentRecord, AgentSpec, DigestRequest, Envelope, ErrorCode, LeaseMode, Request, Response,
    SummarySource,
};
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::{Backend, Client};
use crate::format;

mod codex;
mod input;

const RUNTIME: &str = "claude-code";
/// How much of a transcript's end is read for the `Stop` summary.
const TRANSCRIPT_TAIL: u64 = 64 * 1024;
const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];
#[cfg(test)]
const EDIT_MATCHER: &str = agentdocker_core::runtime::CLAUDE_CODE_EDIT_MATCHER;
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "fish", "ksh"];

#[derive(Args, Debug)]
pub struct HookArgs {
    #[command(subcommand)]
    pub command: HookCommand,
}

#[derive(Subcommand, Debug)]
pub enum HookCommand {
    /// Handle one Claude Code hook event, read as JSON from stdin.
    ClaudeCode(ClaudeCodeArgs),
    /// Report Codex activity and deliver queued messages at lifecycle boundaries.
    Codex,
    /// Write the hook configuration into a host's settings file.
    Install(InstallArgs),
}

#[derive(Args, Debug, Clone)]
pub struct ClaudeCodeArgs {
    /// Seconds an edit lease lasts. Renewed by every edit, released on Stop.
    #[arg(long, default_value_t = 600)]
    pub ttl: u64,
    /// Let the session stop even when other agents' messages are waiting.
    #[arg(long)]
    pub no_wake: bool,
    /// Journal entries a starting session is handed at most.
    #[arg(long, default_value_t = 20)]
    pub digest_entries: usize,
    /// Characters of journal a starting session is handed at most.
    #[arg(long, default_value_t = 2000)]
    pub digest_chars: usize,
    /// New journal entries handed over with a prompt at most.
    #[arg(long, default_value_t = 5)]
    pub prompt_digest_entries: usize,
    /// Characters of new journal handed over with a prompt at most.
    #[arg(long, default_value_t = 500)]
    pub prompt_digest_chars: usize,
}

#[derive(Args, Debug)]
pub struct InstallArgs {
    #[arg(value_enum)]
    pub host: Host,
    /// Use the provider's user configuration root instead of this project's.
    #[arg(long)]
    pub user: bool,
}

#[derive(ValueEnum, Clone, Debug)]
pub enum Host {
    ClaudeCode,
    Codex,
}

/// The fields of a Claude Code hook event this adapter looks at.
#[derive(Deserialize, Debug, Default, Clone)]
pub struct HookInput {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    pub hook_event_name: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<Value>,
    #[serde(default)]
    pub stop_hook_active: bool,
    /// The session's JSONL transcript; its tail is the `Stop` summary.
    #[serde(default)]
    pub transcript_path: Option<PathBuf>,
}

pub async fn run(client: Client, args: HookArgs) -> Result<()> {
    // A hook that has to start the daemon may wait a moment, not stall
    // the editor: it fails open past this.
    let client = client.with_start_timeout(Some(std::time::Duration::from_secs(1)));
    match args.command {
        HookCommand::Codex => {
            if let Err(error) = codex::run(&client).await {
                eprintln!("agentdocker hook codex: {error:#}");
            }
            Ok(())
        }
        HookCommand::Install(install) => install_hooks(&install),
        HookCommand::ClaudeCode(opts) => {
            // Fail open all the way down: an unreadable or malformed event is
            // reported on stderr and Claude Code carries on.
            let input = match read_event() {
                Ok(input) => input,
                Err(err) => {
                    eprintln!("agentdocker hook: {err:#}");
                    return Ok(());
                }
            };
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
            let delivery = HookDelivery {
                backend: &client,
                pending: RefCell::new(Vec::new()),
            };
            let output = match bounded_claude_code_at(&delivery, &input, &opts, deadline).await {
                Ok(output) => output,
                Err(err) => {
                    eprintln!("agentdocker hook ({}): {err:#}", input.hook_event_name);
                    None
                }
            };
            // Lifecycle observations are independent of coordination output:
            // an older daemon or failed activity report cannot discard a
            // lease denial or acknowledge undelivered messages.
            let activity = match input.hook_event_name.as_str() {
                "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => {
                    Some(agentdocker_core::ReportedActivity::Working)
                }
                "Stop" if output.as_ref().is_some_and(|v| v["decision"] == "block") => {
                    Some(agentdocker_core::ReportedActivity::Working)
                }
                "Stop" => Some(agentdocker_core::ReportedActivity::Idle),
                _ => None,
            };
            if let Some(output) = output {
                if let Err(error) =
                    write_output_before(1, format!("{output}\n").as_bytes(), deadline)
                {
                    eprintln!("agentdocker hook: output delivery failed: {error}");
                    return Ok(());
                }
                // Lost acknowledgements cause duplicates, never lost messages.
                let pending = delivery.pending.take();
                for request in pending {
                    let _ = tokio::time::timeout_at(deadline, client.call_raw(&request)).await;
                }
            }
            if let Some(activity) = activity {
                let _ = tokio::time::timeout_at(deadline, async {
                    if let Some(agent) = session_agent(&client, &input).await? {
                        client
                            .call_raw(&Request::ReportActivity {
                                agent: agent.id.to_string(),
                                observation: agentdocker_core::ActivityObservation {
                                    activity,
                                    observed_at: chrono::Utc::now(),
                                },
                            })
                            .await?;
                    }
                    Ok::<_, anyhow::Error>(())
                })
                .await;
            }
            Ok(())
        }
    }
}

/// Read inboxes without consuming them; acknowledge only after output is flushed.
struct HookDelivery<'a, B> {
    backend: &'a B,
    pending: RefCell<Vec<Request>>,
}

impl<B: Backend> Backend for HookDelivery<'_, B> {
    async fn call(&self, request: Request) -> Result<Response> {
        if let Request::Inbox { agent, .. } = request {
            let response = self
                .backend
                .call(Request::Inbox {
                    agent: agent.clone(),
                    drain: false,
                })
                .await?;
            if let Response::Messages { messages } = &response {
                self.pending.borrow_mut().push(Request::AckInbox {
                    agent,
                    messages: messages.iter().map(|m| m.id.clone()).collect(),
                });
            }
            Ok(response)
        } else {
            self.backend.call(request).await
        }
    }
}

/// Bound the entire hook, including reads from a listening but unresponsive daemon.
#[cfg(test)]
async fn bounded_claude_code<B: Backend>(
    backend: &B,
    input: &HookInput,
    opts: &ClaudeCodeArgs,
) -> Result<Option<Value>> {
    bounded_claude_code_at(
        backend,
        input,
        opts,
        tokio::time::Instant::now() + std::time::Duration::from_secs(1),
    )
    .await
}

async fn bounded_claude_code_at<B: Backend>(
    backend: &B,
    input: &HookInput,
    opts: &ClaudeCodeArgs,
    deadline: tokio::time::Instant,
) -> Result<Option<Value>> {
    tokio::time::timeout_at(deadline, claude_code(backend, input, opts))
        .await
        .context("coordination exceeded the one-second hook budget")?
}

/// Deliver output on a nonblocking descriptor within the same hook deadline.
/// A partial/failed delivery is never acknowledged, permitting safe redelivery.
fn write_output_before(
    fd: i32,
    mut bytes: &[u8],
    deadline: tokio::time::Instant,
) -> std::io::Result<()> {
    struct Restore(i32, i32);
    impl Drop for Restore {
        fn drop(&mut self) {
            unsafe {
                libc::fcntl(self.0, libc::F_SETFL, self.1);
            }
        }
    }
    // SAFETY: fd remains borrowed for this call; flags are restored on every exit.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let _restore = Restore(fd, flags);
    while !bytes.is_empty() {
        if tokio::time::Instant::now() >= deadline {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        // SAFETY: bytes is valid for its length, and no ownership of fd is taken.
        let count = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if count > 0 {
            bytes = &bytes[count as usize..];
            continue;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        if error.kind() != std::io::ErrorKind::WouldBlock {
            return Err(error);
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let milliseconds = i32::try_from(remaining.as_millis())
            .unwrap_or(i32::MAX)
            .max(1);
        let mut pollfd = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: one initialized descriptor lives throughout this bounded poll.
        unsafe {
            libc::poll(&mut pollfd, 1, milliseconds);
        }
    }
    Ok(())
}

fn read_event() -> Result<HookInput> {
    input::read(0, std::time::Duration::from_secs(1))
        .context("stdin is not a bounded Claude Code hook event")
}

/// Handle one event. `Some(value)` is JSON for Claude Code's stdout.
pub async fn claude_code<B: Backend>(
    backend: &B,
    input: &HookInput,
    opts: &ClaudeCodeArgs,
) -> Result<Option<Value>> {
    match input.hook_event_name.as_str() {
        "SessionStart" => {
            let me = ensure_registered(backend, input).await?;
            let agents = all_agents(backend).await?;
            let inbox = drain_inbox(backend, &me).await?;
            report_vcs(backend, &me, input).await;
            let mut text = orientation(&me, &agents, &inbox);
            let digest = journal_digest(backend, &me, opts.digest_entries, opts.digest_chars).await;
            if !digest.is_empty() {
                text.push_str("\n\n");
                text.push_str(&journal_text(&digest));
            }
            Ok(Some(context_output("SessionStart", text)))
        }
        "UserPromptSubmit" | "PostToolUse" => {
            let me = ensure_registered(backend, input).await?;
            if input.hook_event_name == "PostToolUse" {
                if let Some(path) = edited_path(input) {
                    backend
                        .call(Request::Observe {
                            agent: me.id.to_string(),
                            paths: vec![path.to_string_lossy().into_owned()],
                        })
                        .await?;
                }
            }
            let inbox = drain_inbox(backend, &me).await?;
            report_vcs(backend, &me, input).await;
            let agents = if inbox.is_empty() {
                Vec::new()
            } else {
                all_agents(backend).await?
            };
            // Only a prompt carries journal text; a tool result never does.
            let digest = if input.hook_event_name == "UserPromptSubmit" {
                journal_digest(
                    backend,
                    &me,
                    opts.prompt_digest_entries,
                    opts.prompt_digest_chars,
                )
                .await
            } else {
                String::new()
            };
            if inbox.is_empty() && digest.is_empty() {
                return Ok(None);
            }
            let mut text = String::new();
            if !inbox.is_empty() {
                text = format!(
                    "AgentDocker: {} new message(s) from other agents:\n{}",
                    inbox.len(),
                    messages_text(&inbox, &agents)
                );
            }
            if !digest.is_empty() {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(&journal_text(&digest));
            }
            Ok(Some(context_output(&input.hook_event_name, text)))
        }
        "PreToolUse" => {
            if let Some(path) = read_path(input) {
                let me = ensure_registered(backend, input).await?;
                backend
                    .call(Request::Observe {
                        agent: me.id.to_string(),
                        paths: vec![path.to_string_lossy().into_owned()],
                    })
                    .await?;
                return Ok(None);
            }
            let Some(path) = edited_path(input) else {
                return Ok(None);
            };
            let me = ensure_registered(backend, input).await?;
            match backend
                .call(Request::Stale {
                    agent: me.id.to_string(),
                    paths: vec![path.to_string_lossy().into_owned()],
                })
                .await?
            {
                Response::Stale { stale } if !stale.is_empty() => {
                    return Ok(Some(json!({
                        "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "deny",
                            "permissionDecisionReason": format!("AgentDocker: your context is stale. Read {} again before editing. {}", path.display(), stale.iter().map(|s| s.reason.as_str()).collect::<Vec<_>>().join("; ")) }
                    })));
                }
                Response::Error { message, .. } => {
                    eprintln!(
                        "agentdocker hook: staleness check failed: {message}; continuing lease protection"
                    );
                }
                _ => {}
            }
            let response = backend
                .call(Request::Claim {
                    agent: me.id.to_string(),
                    resource: format!("path:{}", path.display()),
                    mode: LeaseMode::Exclusive,
                    amount: None,
                    ttl_secs: opts.ttl,
                    note: Some(format!("editing in Claude Code session {}", me.spec.name)),
                    wait_secs: 0,
                })
                .await?;
            match response {
                Response::Error {
                    code: ErrorCode::Conflict,
                    message,
                    details,
                } => Ok(Some(deny_output(&path, &message, details.as_ref()))),
                Response::Error { message, .. } => {
                    eprintln!(
                        "agentdocker hook: could not claim {}: {message}",
                        path.display()
                    );
                    Ok(None)
                }
                _ => Ok(None),
            }
        }
        "Stop" => {
            let Some(me) = session_agent(backend, input).await? else {
                return Ok(None);
            };
            // What the model last said is what the release entry quotes.
            let summary = input
                .transcript_path
                .as_deref()
                .and_then(transcript_tail)
                .and_then(|tail| transcript_summary(&tail));
            release_all(backend, &me, summary).await?;
            if opts.no_wake || input.stop_hook_active {
                return Ok(None);
            }
            let inbox = drain_inbox(backend, &me).await?;
            if inbox.is_empty() {
                return Ok(None);
            }
            let agents = all_agents(backend).await?;
            Ok(Some(json!({
                "decision": "block",
                "reason": format!(
                    "AgentDocker: {} message(s) from other agents arrived while you were working. \
                     Read and act on them before finishing (reply with `agentdocker send --to <agent> \"...\"`):\n{}",
                    inbox.len(),
                    messages_text(&inbox, &agents)
                ),
            })))
        }
        "SessionEnd" => {
            if let Some(me) = session_agent(backend, input).await? {
                release_all(backend, &me, None).await?;
                backend
                    .call(Request::Deregister {
                        agent: me.id.to_string(),
                    })
                    .await?;
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

/// `claude-<first 8 chars of the session id>`: stable across hook
/// invocations of one session, and what `agentdocker ps` shows.
pub fn session_name(session_id: &str) -> String {
    let end = session_id
        .char_indices()
        .nth(8)
        .map_or(session_id.len(), |(i, _)| i);
    format!("claude-{}", &session_id[..end])
}

/// The path an editing tool is about to touch, made absolute and canonical
/// so it lines up with what other agents claim.
fn read_path(input: &HookInput) -> Option<PathBuf> {
    let tool = input.tool_name.as_deref()?;
    if !["Read", "Grep", "Glob"].contains(&tool) {
        return None;
    }
    let value = input.tool_input.as_ref();
    if tool == "Read"
        && value
            .and_then(|v| v.get("file_path"))
            .and_then(Value::as_str)
            .is_none()
    {
        return None;
    }
    let raw = value
        .and_then(|v| v.get("file_path").or_else(|| v.get("path")))
        .and_then(Value::as_str)
        .unwrap_or(".");
    let base = input.cwd.clone().or_else(|| std::env::current_dir().ok())?;
    Some(agentdocker_host::project::canonical(&base.join(raw)))
}

pub fn edited_path(input: &HookInput) -> Option<PathBuf> {
    let tool = input.tool_name.as_deref()?;
    if !EDIT_TOOLS.contains(&tool) {
        return None;
    }
    let tool_input = input.tool_input.as_ref()?;
    let raw = tool_input
        .get("file_path")
        .or_else(|| tool_input.get("notebook_path"))
        .and_then(Value::as_str)?;
    let path = PathBuf::from(raw);
    let absolute = if path.is_absolute() {
        path
    } else {
        match &input.cwd {
            Some(cwd) => cwd.join(path),
            None => std::env::current_dir().ok()?.join(path),
        }
    };
    Some(normalize(&absolute))
}

/// Canonicalize what exists; for a file about to be created, canonicalize
/// its parent and keep the file name.
pub fn normalize(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => match parent.canonicalize() {
            Ok(parent) => parent.join(name),
            Err(_) => path.to_path_buf(),
        },
        _ => path.to_path_buf(),
    }
}

async fn current_agent<B: Backend>(backend: &B, input: &HookInput) -> Result<Option<AgentRecord>> {
    let reference = std::env::var("AGENTDOCKER_AGENT_ID")
        .ok()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| session_name(&input.session_id));
    match backend.call(Request::Inspect { agent: reference }).await? {
        Response::Agent { agent } if agent.status.is_live() => Ok(Some(agent)),
        _ => Ok(None),
    }
}

/// This session's agent, under whichever name it ended up with.
///
/// One process is one agent now, and the MCP server registers for the
/// same process under a name taken from the pid. Whichever half gets
/// there first owns the name, so when the MCP server wins, looking up
/// `claude-<session>` finds nothing — and `Stop` and `SessionEnd` would
/// skip the releases and the deregistration they exist to do, leaking
/// every lease the session held. The pid is what the two halves agree
/// on, so it is what finds the other one.
///
/// Lifecycle and activity events use this lookup. Registration also
/// validates a named record before reusing it; the daemon resolves an
/// unmatched registration against the same physical session identity.
async fn session_agent<B: Backend>(backend: &B, input: &HookInput) -> Result<Option<AgentRecord>> {
    found_by_pid(backend, input, host_pid()).await
}

/// The pid is taken rather than read so a fixture can supply one.
/// `host_pid` walks real ancestry and is allowed to decline, and a test
/// that skipped its assertion when it did would not be testing anything.
async fn found_by_pid<B: Backend>(
    backend: &B,
    input: &HookInput,
    pid: Option<u32>,
) -> Result<Option<AgentRecord>> {
    // An explicit binding is authoritative. Whoever started this agent
    // set `AGENTDOCKER_AGENT_ID` and knows which record it is, and that
    // record's pid may legitimately not be this process — `run`
    // registers an agent for the child it spawns. A name *derived* from
    // the session id is a different thing entirely, and is checked below.
    if let Some(bound) = std::env::var("AGENTDOCKER_AGENT_ID")
        .ok()
        .filter(|id| !id.is_empty())
    {
        return match backend.call(Request::Inspect { agent: bound }).await? {
            Response::Agent { agent } if agent.status.is_live() => Ok(Some(agent)),
            _ => Ok(None),
        };
    }
    let named = current_agent(backend, input).await?;
    // Unverifiable ancestry authorises nothing.
    //
    // What this answer is used for is releasing an agent's leases and
    // deregistering it. Without a pid, or without a birth time to tell a
    // recycled pid from the original, there is nothing to check a record
    // against — and a name is not proof: it outlives the session that
    // chose it, and eight characters of session id is not much to
    // collide. Ending nothing costs an expiry; ending the wrong agent
    // costs somebody their work.
    let Some(pid) = pid else {
        return Ok(None);
    };
    let Some(started) = agentdocker_host::procinfo::start_time(pid) else {
        return Ok(None);
    };
    // Resolved once, and before the listing: canonicalising touches the
    // filesystem, and doing it per candidate inside the comparison would
    // make the check cost grow with the fleet.
    let Some(here) = input.cwd.as_ref().and_then(|cwd| cwd.canonicalize().ok()) else {
        return Ok(None);
    };
    // The same predicate the daemon registers by, for the same reason:
    // this hook is about to release another agent's leases and
    // deregister it, so "shares a pid" is nowhere near enough. A
    // recycled pid, another runtime in one host, another project, or a
    // second session multiplexed into this process are each a different
    // agent, and ending one of those instead would be worse than ending
    // nothing.
    let ours =
        |agent: &AgentRecord| same_hook_session(agent, &input.session_id, pid, started, &here);
    // The name is a hint, not proof. A session id prefix is eight
    // characters and a name outlives the session that chose it, so a
    // live record answering to it may be a different process entirely —
    // it still has to pass.
    if let Some(named) = named.filter(&ours) {
        return Ok(Some(named));
    }
    match backend
        .call(Request::List {
            all: false,
            // Narrowed to this session's own project, and resolved by
            // the daemon from the directory rather than compared here:
            // a project spans its main checkout and every linked
            // worktree, and only the daemon knows which is which.
            project: input.cwd.as_ref().map(|cwd| cwd.display().to_string()),
            labels: Default::default(),
        })
        .await?
    {
        Response::Agents { agents } => {
            let mut matching = agents.into_iter().filter(ours);
            let first = matching.next();
            Ok(if matching.next().is_none() {
                first
            } else {
                None
            })
        }
        _ => Ok(None),
    }
}

fn same_hook_session(
    agent: &AgentRecord,
    session: &str,
    pid: u32,
    started: chrono::DateTime<chrono::Utc>,
    workdir: &Path,
) -> bool {
    agent.status.is_live()
        && agent.pid == Some(pid)
        && agent.process_started_at == Some(started)
        && agent.spec.runtime == RUNTIME
        && agent
            .spec
            .labels
            .get("session_id")
            .filter(|id| !id.is_empty())
            .is_none_or(|theirs| theirs == session)
        && agent
            .spec
            .workdir
            .as_ref()
            .is_some_and(|theirs| theirs.canonicalize().is_ok_and(|theirs| theirs == workdir))
}

async fn ensure_registered<B: Backend>(backend: &B, input: &HookInput) -> Result<AgentRecord> {
    if let Some(me) = current_agent(backend, input).await? {
        let explicit = std::env::var("AGENTDOCKER_AGENT_ID")
            .ok()
            .is_some_and(|id| !id.is_empty());
        if explicit {
            return Ok(me);
        }
        let verified = host_pid()
            .and_then(|pid| {
                let started = agentdocker_host::procinfo::start_time(pid)?;
                let here = input.cwd.as_ref()?.canonicalize().ok()?;
                Some(same_hook_session(
                    &me,
                    &input.session_id,
                    pid,
                    started,
                    &here,
                ))
            })
            .unwrap_or(false);
        if verified {
            return Ok(me);
        }
    }
    let mut labels = std::collections::BTreeMap::from([
        ("via".to_owned(), "hook".to_owned()),
        ("session_id".to_owned(), input.session_id.clone()),
    ]);
    if let Some(source) = &input.source {
        labels.insert("source".to_owned(), source.clone());
    }
    let spec = AgentSpec {
        name: session_name(&input.session_id),
        runtime: RUNTIME.to_owned(),
        workdir: input.cwd.clone(),
        labels,
        ..AgentSpec::default()
    };
    match backend
        .call(Request::Register {
            spec,
            pid: host_pid(),
            // Read here rather than by the daemon: this process is
            // *inside* whatever session it is reporting, which is
            // first-hand — and on macOS the only way to know, since
            // a process's environment is not readable from outside.
            session: agentdocker_host::multiplexer::own(),
        })
        .await?
    {
        Response::Agent { agent } => Ok(agent),
        Response::Error { message, .. } => bail!("registration refused: {message}"),
        other => bail!("unexpected reply to register: {other:?}"),
    }
}

/// The pid of the Claude Code process, for the daemon's liveness check.
/// Hooks run under a shell, so walk up past any shells to the first real
/// ancestor. `None` if that can't be worked out; the agent then relies on
/// `SessionEnd` to leave.
fn host_pid() -> Option<u32> {
    let mut pid = parent_id();
    for _ in 0..6 {
        let (ppid, comm) = parent_of(pid)?;
        let name = Path::new(&comm)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&comm)
            .trim_start_matches('-')
            .to_owned();
        if !SHELLS.contains(&name.as_str()) {
            return Some(pid);
        }
        pid = ppid;
    }
    None
}

fn parent_of(pid: u32) -> Option<(u32, String)> {
    let output = std::process::Command::new("ps")
        .args(["-o", "ppid=,comm=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut parts = text.split_whitespace();
    let ppid = parts.next()?.parse().ok()?;
    let comm = parts.collect::<Vec<_>>().join(" ");
    Some((ppid, comm))
}

/// Tell the daemon which branch and commit the session's directory is on.
/// Two file reads, no `git` process; the daemon ignores an unchanged
/// state, and any failure is silent because it is only an observation.
async fn report_vcs<B: Backend>(backend: &B, me: &AgentRecord, input: &HookInput) {
    let Some(vcs) = input.cwd.as_deref().and_then(agentdocker_host::vcs::state) else {
        return;
    };
    let _ = backend
        .call(Request::Report {
            agent: me.id.to_string(),
            vcs: Some(vcs),
        })
        .await;
}

async fn all_agents<B: Backend>(backend: &B) -> Result<Vec<AgentRecord>> {
    match backend
        .call(Request::List {
            all: true,
            project: None,
            labels: Default::default(),
        })
        .await?
    {
        Response::Agents { agents } => Ok(agents),
        _ => Ok(Vec::new()),
    }
}

async fn drain_inbox<B: Backend>(backend: &B, me: &AgentRecord) -> Result<Vec<Envelope>> {
    match backend
        .call(Request::Inbox {
            agent: me.id.to_string(),
            drain: true,
        })
        .await?
    {
        Response::Messages { messages } => Ok(messages),
        _ => Ok(Vec::new()),
    }
}

/// Release everything; a summary here is a quoted transcript tail, never
/// something the model typed for the journal.
async fn release_all<B: Backend>(
    backend: &B,
    me: &AgentRecord,
    summary: Option<String>,
) -> Result<()> {
    let summary_source = if summary.is_some() {
        SummarySource::Transcript
    } else {
        SummarySource::Explicit
    };
    backend
        .call(Request::ReleaseAll {
            agent: me.id.to_string(),
            summary,
            summary_source,
        })
        .await?;
    Ok(())
}

/// The project journal since this agent last looked, marked as seen.
/// Empty when nothing is new, when the agent is in no project, or when
/// the daemon could not say: a hook fails open.
async fn journal_digest<B: Backend>(
    backend: &B,
    me: &AgentRecord,
    max_entries: usize,
    max_chars: usize,
) -> String {
    if me.project.is_none() || max_entries == 0 || max_chars == 0 {
        return String::new();
    }
    let request = Request::Journal {
        project: String::new(),
        since_seq: None,
        until_seq: None,
        agent: None,
        branch: None,
        kind: None,
        path: None,
        grep: None,
        limit: max_entries,
        digest: Some(DigestRequest {
            reader: me.id.to_string(),
            max_entries,
            max_chars,
            all_branches: false,
            advance: true,
        }),
    };
    match backend.call(request).await {
        Ok(Response::Digest { digest, .. }) => digest.text,
        Ok(Response::Error { message, .. }) => {
            eprintln!("agentdocker hook: journal digest failed: {message}");
            String::new()
        }
        Ok(_) => String::new(),
        Err(err) => {
            eprintln!("agentdocker hook: journal digest failed: {err:#}");
            String::new()
        }
    }
}

fn journal_text(digest: &str) -> String {
    format!("AgentDocker journal (what changed in this project and why):\n{digest}")
}

/// The last [`TRANSCRIPT_TAIL`] bytes of a transcript, minus the line the
/// cut fell in. Cost does not grow with the transcript.
fn transcript_tail(path: &Path) -> Option<String> {
    use std::io::{Seek, SeekFrom};
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    let len = metadata.len();
    let start = len.saturating_sub(TRANSCRIPT_TAIL);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(TRANSCRIPT_TAIL).read_to_end(&mut bytes).ok()?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0 {
        let cut = text.find('\n')?;
        text.drain(..=cut);
    }
    Some(text)
}

fn context_output(event: &str, text: String) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": event,
            "additionalContext": text,
        }
    })
}

fn deny_output(path: &Path, message: &str, details: Option<&Value>) -> Value {
    let mut reason = format!(
        "AgentDocker: another agent holds a lease on {} — {message}.",
        path.display()
    );
    if let Some(holders) = details
        .and_then(|d| d.get("held_by"))
        .and_then(Value::as_array)
    {
        for holder in holders {
            if let Some(note) = holder.get("note").and_then(Value::as_str) {
                reason.push_str(&format!(" Their note: \"{note}\"."));
            }
        }
    }
    reason.push_str(
        " Do not edit this file now. Message the holder with \
         `agentdocker send --to <agent> \"...\"`, wait for the lease to expire, \
         or work on something else.",
    );
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    })
}

fn display_name(id: &str, agents: &[AgentRecord]) -> String {
    agents
        .iter()
        .find(|a| a.id.as_str() == id)
        .map_or_else(|| id.to_owned(), |a| a.spec.name.clone())
}

fn messages_text(inbox: &[Envelope], agents: &[AgentRecord]) -> String {
    inbox
        .iter()
        .map(|m| {
            format!(
                "- [{}] {} [{}]: {}",
                format::clock(m.sent_at),
                display_name(&m.from, agents),
                m.kind,
                format::payload_text(&m.payload)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn orientation(me: &AgentRecord, agents: &[AgentRecord], inbox: &[Envelope]) -> String {
    use agentdocker_core::ProjectRef;

    let describe = |a: &AgentRecord| {
        format!(
            "{} ({}{})",
            a.spec.name,
            a.spec.runtime,
            a.spec
                .model
                .as_ref()
                .map(|m| format!(", {m}"))
                .unwrap_or_default()
        )
    };
    let mine = me.project.as_ref().map(ProjectRef::id);
    let (here, elsewhere): (Vec<String>, Vec<String>) = agents
        .iter()
        .filter(|a| a.id != me.id && a.status.is_live())
        .partition_map_by(
            |a| mine.is_some() && a.project.as_ref().map(ProjectRef::id) == mine,
            describe,
        );

    let mut text = format!("AgentDocker: this session is agent `{}`", me.spec.name);
    if let Some(project) = &me.project {
        text.push_str(&format!(" in project `{}`", project.name()));
    }
    text.push_str(". ");
    match (here.is_empty(), elsewhere.is_empty()) {
        (true, true) => text.push_str("No other agents are live right now. "),
        (true, false) if mine.is_none() => {
            text.push_str(&format!("Other live agents: {}. ", elsewhere.join(", ")));
        }
        _ => {
            if !here.is_empty() {
                text.push_str(&format!("In this project: {}. ", here.join(", ")));
            }
            if !elsewhere.is_empty() {
                text.push_str(&format!("Elsewhere: {}. ", elsewhere.join(", ")));
            }
        }
    }
    text.push_str(
        "Edits are leased automatically: if another agent holds a file, the edit is refused \
         with their name and note — coordinate instead of retrying. Talk to an agent with \
         `agentdocker send --to <name> \"<text>\"`, or to everyone in this project with \
         `--to project`; their replies are handed to you here as they arrive. \
         `agentdocker ps` and `agentdocker leases` show the current state.",
    );
    if !inbox.is_empty() {
        text.push_str(&format!(
            "\nMessages waiting ({}):\n{}",
            inbox.len(),
            messages_text(inbox, agents)
        ));
    }
    text
}

/// `partition` for iterators of references, mapping as it goes.
trait PartitionMapBy: Iterator + Sized {
    fn partition_map_by<T>(
        self,
        pick: impl Fn(&Self::Item) -> bool,
        map: impl Fn(Self::Item) -> T,
    ) -> (Vec<T>, Vec<T>) {
        let mut yes = Vec::new();
        let mut no = Vec::new();
        for item in self {
            if pick(&item) {
                yes.push(map(item));
            } else {
                no.push(map(item));
            }
        }
        (yes, no)
    }
}

impl<I: Iterator> PartitionMapBy for I {}

// ----- install --------------------------------------------------------------

pub(crate) fn install_hooks(args: &InstallArgs) -> Result<()> {
    let runtime = match args.host {
        Host::ClaudeCode => "claude-code",
        Host::Codex => "codex",
    };
    let path = if args.user {
        agentdocker_host::runtimes::hook_config_path(
            agentdocker_core::runtime::spec(runtime).expect("supported hook runtime"),
            &agentdocker_host::runtimes::Roots::from_env(),
        )
    } else if runtime == "codex" {
        PathBuf::from(".codex/hooks.json")
    } else {
        PathBuf::from(".claude").join("settings.json")
    };
    let existing = crate::setup::guided::read_config(&path)?;
    let mut settings: Value = match existing.as_deref() {
        Some(raw) => serde_json::from_str(raw)
            .with_context(|| format!("{} is not valid JSON", path.display()))?,
        None => json!({}),
    };
    let exe = crate::desktop::setup_executable()
        .context("cannot locate the active agentdocker binary")?;
    let command = agentdocker_host::runtimes::hook_command(&exe, runtime)?;
    let added = merge_hooks(&mut settings, &command, runtime)?;
    if added == 0 {
        // Nothing to add, so leave the file byte-for-byte alone: a rewrite
        // would re-sort and re-indent the user's whole settings document.
        eprintln!("{}: hooks already installed", path.display());
        return Ok(());
    }
    crate::setup::write_config(
        &path,
        existing.as_deref(),
        &format!("{}\n", serde_json::to_string_pretty(&settings)?),
    )
    .with_context(|| format!("cannot write {}", path.display()))?;
    if runtime == "codex" {
        eprintln!(
            "Codex hooks deliver queued messages and require review/trust in /hooks. MCP supplies coordination tools. Existing sessions may need to be resumed to load configuration."
        );
    }
    eprintln!(
        "{}: added {added} hook entries running `{command}`",
        path.display()
    );
    Ok(())
}

/// Add our hook entries to a Claude Code settings document. Entries whose
/// command already runs `hook claude-code` are left alone, so this is safe
/// to run repeatedly. Returns how many entries were added.
pub fn merge_claude_code_hooks(settings: &mut Value, command: &str) -> Result<usize> {
    merge_hooks(settings, command, "claude-code")
}

pub(super) fn merge_hooks(settings: &mut Value, command: &str, runtime: &str) -> Result<usize> {
    let root = settings
        .as_object_mut()
        .context("settings must be a JSON object")?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("`hooks` must be a JSON object")?;
    let mut added = 0;
    let managed = |hook: &Value| {
        hook["type"] == json!("command")
            && hook["command"].as_str().is_some_and(|c| {
                agentdocker_host::runtimes::hook_command_matches_for(c, "agentdocker", runtime)
            })
    };
    for (event, matcher) in agentdocker_host::runtimes::hook_events(runtime) {
        let entries = hooks
            .entry(*event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .with_context(|| format!("`hooks.{event}` must be an array"))?;
        if runtime == "codex" && *event == "Interrupt" {
            for entry in entries.iter_mut() {
                if let Some(hooks) = entry["hooks"].as_array_mut() {
                    for hook in hooks.iter_mut().filter(|hook| managed(hook)) {
                        if hook["timeout"]
                            .as_f64()
                            .is_some_and(|seconds| seconds > 3.0)
                        {
                            hook["timeout"] = json!(3);
                            added += 1;
                        }
                    }
                }
            }
        }
        let present = entries.iter().any(|entry| {
            entry["hooks"]
                .as_array()
                .is_some_and(|hooks| hooks.iter().any(managed))
        });
        if present {
            let mut upgraded = Vec::new();
            for entry in entries.iter_mut() {
                let Some(hooks) = entry["hooks"].as_array() else {
                    continue;
                };
                let actual = entry.get("matcher").and_then(Value::as_str);
                let covers = match matcher {
                    Some(expected) => actual == Some(*expected) || actual == Some("*"),
                    None => actual.is_none() || actual == Some(""),
                };
                if !hooks.iter().any(managed) || covers {
                    continue;
                }
                let (ours, others): (Vec<_>, Vec<_>) = hooks.iter().cloned().partition(managed);
                let set_matcher = |entry: &mut Value| {
                    if let Some(matcher) = matcher {
                        entry["matcher"] = json!(matcher);
                    } else if let Some(object) = entry.as_object_mut() {
                        object.remove("matcher");
                    }
                };
                if others.is_empty() {
                    set_matcher(entry);
                } else {
                    // Repair only our coverage; other hooks retain their scope.
                    let mut separate = entry.clone();
                    separate["hooks"] = json!(ours);
                    set_matcher(&mut separate);
                    entry["hooks"] = json!(others);
                    upgraded.push(separate);
                }
                added += 1;
            }
            entries.extend(upgraded);
            continue;
        }
        let timeout = if runtime == "codex" && *event == "Interrupt" {
            3
        } else {
            15
        };
        let mut entry = json!({
            "hooks": [{ "type": "command", "command": command, "timeout": timeout }]
        });
        if let Some(matcher) = matcher {
            entry["matcher"] = json!(matcher);
        }
        entries.push(entry);
        added += 1;
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use agentdocker_core::{AgentId, AgentStatus, Destination};
    use chrono::Utc;

    use super::*;
    use crate::client::mock::Mock;

    /// A live agent has a process, and since the lifecycle hooks verify
    /// the record against one before releasing its leases, a fixture
    /// without one is not a live agent — it is a record nothing can
    /// confirm. This test process is the one to hand.
    fn agent(name: &str, live: bool) -> AgentRecord {
        let mut record = AgentRecord::new(
            AgentSpec {
                name: name.to_owned(),
                runtime: RUNTIME.to_owned(),
                ..AgentSpec::default()
            },
            false,
            Utc::now(),
        );
        record.status = if live {
            AgentStatus::Running
        } else {
            AgentStatus::Exited { code: Some(0) }
        };
        if live {
            let me = fixture_pid();
            record.pid = Some(me);
            record.process_started_at = agentdocker_host::procinfo::start_time(me);
            // The checkout the fixture session is in. A live agent the
            // lifecycle hooks will act on has to be verifiably in the
            // same tree, so a fixture without one is not a live agent
            // they would touch.
            record.spec.workdir = Some(std::env::temp_dir());
        }
        record
    }

    /// The pid the lifecycle hooks will actually look for.
    ///
    /// `session_agent` asks `host_pid`, which walks real ancestry to the
    /// first non-shell parent — not this process. A fixture built on
    /// `std::process::id()` therefore fails the very check it is meant
    /// to pass, and the hook falls through to a listing nobody mocked.
    fn fixture_pid() -> u32 {
        host_pid().unwrap_or_else(std::process::id)
    }

    fn agent_with_pid(name: &str) -> AgentRecord {
        agent(name, true)
    }

    fn message(from: &str, text: &str) -> Envelope {
        Envelope::new(
            from,
            Destination::Agent(AgentId::from("me")),
            "chat",
            json!({ "text": text }),
            None,
            Utc::now(),
        )
    }

    fn input(event: &str) -> HookInput {
        HookInput {
            session_id: "0123456789abcdef".into(),
            cwd: Some(std::env::temp_dir()),
            hook_event_name: event.into(),
            ..HookInput::default()
        }
    }

    fn opts() -> ClaudeCodeArgs {
        ClaudeCodeArgs {
            ttl: 600,
            no_wake: false,
            digest_entries: 20,
            digest_chars: 2000,
            prompt_digest_entries: 5,
            prompt_digest_chars: 500,
        }
    }

    fn digest_reply(text: &str) -> Response {
        use agentdocker_core::{Digest, ProjectRef};
        Response::Digest {
            project: ProjectRef::directory("/work/alpha").id(),
            digest: Digest {
                text: text.to_owned(),
                head_seq: 9,
                shown: usize::from(!text.is_empty()),
                collapsed: 0,
                other_branches: 0,
            },
        }
    }

    #[tokio::test]
    async fn a_named_record_from_another_process_does_not_bypass_registration() {
        let event = input("SessionStart");
        let mut old = agent(&session_name(&event.session_id), true);
        old.pid = Some(u32::MAX);
        let current = agent(&session_name(&event.session_id), true);
        let backend = Mock::with(vec![
            Response::Agent { agent: old },
            Response::Agent {
                agent: current.clone(),
            },
        ]);
        let registered = ensure_registered(&backend, &event).await.unwrap();
        assert_eq!(registered.id, current.id);
        assert!(matches!(backend.requests()[1], Request::Register { .. }));
    }

    #[tokio::test]
    async fn session_start_and_prompts_carry_the_journal_digest() {
        use agentdocker_core::ProjectRef;
        let mut me = agent("claude-01234567", true);
        me.project = Some(ProjectRef::directory("/work/alpha"));
        let backend = Mock::with(vec![
            Response::Agent { agent: me.clone() },
            Response::Agents {
                agents: vec![me.clone()],
            },
            Response::Messages {
                messages: Vec::new(),
            },
            digest_reply(
                "Since you last looked (1 entry):\n- 4m ago   codex-1 [main] noted: \"parser is next\"\n",
            ),
        ]);
        let out = claude_code(&backend, &input("SessionStart"), &opts())
            .await
            .unwrap()
            .unwrap();
        let text = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(text.contains("agent `claude-01234567`"), "{text}");
        assert!(text.contains("AgentDocker journal"), "{text}");
        assert!(text.contains("parser is next"), "{text}");
        assert!(matches!(
            &backend.requests()[3],
            Request::Journal { project, digest: Some(d), .. }
                if project.is_empty() && d.reader == me.id.as_str() && d.advance
                    && d.max_entries == 20 && d.max_chars == 2000
        ));

        // A prompt: the digest alone is worth speaking for, within the
        // smaller budget.
        let backend = Mock::with(vec![
            Response::Agent { agent: me.clone() },
            Response::Messages {
                messages: Vec::new(),
            },
            digest_reply(
                "Since you last looked (1 entry):\n- 1m ago   codex-1 [main] noted: \"lexer done\"\n",
            ),
        ]);
        let out = claude_code(&backend, &input("UserPromptSubmit"), &opts())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            out["hookSpecificOutput"]["hookEventName"],
            "UserPromptSubmit"
        );
        let text = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(text.starts_with("AgentDocker journal"), "{text}");
        assert!(text.contains("lexer done"), "{text}");
        assert!(matches!(
            &backend.requests()[2],
            Request::Journal { digest: Some(d), .. } if d.max_entries == 5 && d.max_chars == 500
        ));

        // Messages and journal together, messages first.
        let backend = Mock::with(vec![
            Response::Agent { agent: me.clone() },
            Response::Messages {
                messages: vec![message("someone", "hi")],
            },
            Response::Agents { agents: vec![] },
            digest_reply(
                "Since you last looked (1 entry):\n- 1m ago   codex-1 [main] noted: \"x\"\n",
            ),
        ]);
        let out = claude_code(&backend, &input("UserPromptSubmit"), &opts())
            .await
            .unwrap()
            .unwrap();
        let text = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(text.find("new message").unwrap() < text.find("AgentDocker journal").unwrap());

        // Nothing new and no messages: silence.
        let backend = Mock::with(vec![
            Response::Agent { agent: me.clone() },
            Response::Messages {
                messages: Vec::new(),
            },
            digest_reply(""),
        ]);
        assert!(
            claude_code(&backend, &input("UserPromptSubmit"), &opts())
                .await
                .unwrap()
                .is_none()
        );
        // No project: the journal is not even asked for.
        let backend = Mock::with(vec![
            Response::Agent {
                agent: agent("claude-01234567", true),
            },
            Response::Messages {
                messages: Vec::new(),
            },
        ]);
        assert!(
            claude_code(&backend, &input("UserPromptSubmit"), &opts())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(backend.requests().len(), 2);
    }

    #[tokio::test]
    async fn stop_quotes_the_transcript_tail_as_the_release_summary() {
        let me = agent("claude-01234567", true);
        let path = std::env::temp_dir().join(format!(
            "agentdocker-hook-test-{}.jsonl",
            std::process::id()
        ));
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Old.\"}]}}\n",
                "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Rewrote the **tokenizer**.\\n\\nMore later.\"}]}}\n",
                "{\"type\":\"user\",\"message\":{\"content\":\"thanks\"}}\n",
            ),
        )
        .unwrap();
        let backend = Mock::with(vec![
            Response::Agent { agent: me.clone() },
            Response::Leases { leases: vec![] },
        ]);
        let mut ev = input("Stop");
        ev.stop_hook_active = true;
        ev.transcript_path = Some(path.clone());
        claude_code(&backend, &ev, &opts()).await.unwrap();
        std::fs::remove_file(&path).ok();
        assert!(
            matches!(
                &backend.requests()[1],
                Request::ReleaseAll { summary: Some(s), summary_source: SummarySource::Transcript, .. }
                    if s == "Rewrote the tokenizer."
            ),
            "{:?}",
            backend.requests()[1]
        );

        // No transcript, or an unreadable one: a plain release.
        let backend = Mock::with(vec![
            Response::Agent { agent: me.clone() },
            Response::Leases { leases: vec![] },
        ]);
        ev.transcript_path = Some(path);
        claude_code(&backend, &ev, &opts()).await.unwrap();
        assert!(matches!(
            &backend.requests()[1],
            Request::ReleaseAll {
                summary: None,
                summary_source: SummarySource::Explicit,
                ..
            }
        ));
    }

    /// A session whose agent is named by the other half is still found.
    ///
    /// One process is one agent, and whichever half registers first owns
    /// the name. When the MCP server won, this adapter looked up
    /// `claude-<session>`, found nothing, and `Stop` and `SessionEnd`
    /// then skipped the releases and the deregistration they exist to
    /// do — every lease the session held would have leaked. The pid is
    /// what the two halves agree on.
    #[tokio::test]
    async fn the_session_is_found_whichever_half_registered_first() {
        let input = input("Stop");

        // Our own name: answered by the first lookup, no listing needed.
        let me = fixture_pid();
        let matching = agent_with_pid;
        let ours = matching(&session_name(&input.session_id));
        let backend = Mock::with(vec![Response::Agent {
            agent: ours.clone(),
        }]);
        let found = found_by_pid(&backend, &input, Some(me)).await.unwrap();
        assert_eq!(found.unwrap().id, ours.id);
        assert_eq!(
            backend.requests().len(),
            1,
            "one lookup when the name checks out"
        );

        // The MCP server's name, and the pid to match. The first lookup
        // misses and the listing finds it. The pid is injected rather
        // than read, so this asserts on every machine.
        let theirs = matching("claude-code-4242");
        let backend = Mock::with(vec![
            Response::error(agentdocker_core::ErrorCode::NotFound, "no such agent"),
            Response::Agents {
                // A genuinely different process, which is the only kind
                // of "somebody else" there can be once one process is
                // one agent.
                agents: vec![
                    {
                        let mut other = agent("somebody-else", true);
                        other.pid = Some(1);
                        other
                    },
                    theirs.clone(),
                ],
            },
        ]);
        let found = found_by_pid(&backend, &input, Some(me)).await.unwrap();
        assert_eq!(found.unwrap().id, theirs.id, "found by pid");

        let backend = Mock::with(vec![
            Response::error(agentdocker_core::ErrorCode::NotFound, "no such agent"),
            Response::Agents {
                agents: vec![theirs.clone(), matching("legacy-duplicate")],
            },
        ]);
        assert!(
            found_by_pid(&backend, &input, Some(me))
                .await
                .unwrap()
                .is_none(),
            "ambiguous legacy identities must not authorize release or deregistration"
        );

        // Everything that shares the pid and is still not this session.
        // Ending any of these instead would be worse than ending none.
        let recycled = {
            let mut a = matching("same-pid-older-process");
            a.process_started_at = Some(Utc::now() - chrono::Duration::hours(24 * 30));
            a
        };
        let other_runtime = {
            let mut a = matching("codex-in-the-same-host");
            a.spec.runtime = "codex".to_owned();
            a
        };
        let other_session = {
            let mut a = matching("claude-another-session");
            a.spec
                .labels
                .insert("session_id".to_owned(), "a-different-session".to_owned());
            a
        };
        for impostor in [recycled, other_runtime, other_session] {
            let name = impostor.spec.name.clone();
            let backend = Mock::with(vec![
                Response::error(agentdocker_core::ErrorCode::NotFound, "no such agent"),
                Response::Agents {
                    agents: vec![impostor],
                },
            ]);
            assert!(
                found_by_pid(&backend, &input, Some(me))
                    .await
                    .unwrap()
                    .is_none(),
                "{name} shares the pid and is not this session"
            );
        }

        // A name that resolves is a hint, not proof. The fast path used
        // to return whatever answered to `claude-<session>` without
        // checking anything: a name outlives the session that chose it,
        // and eight characters of session id is not a lot.
        let impostor_by_name = {
            let mut a = agent(&session_name(&input.session_id), true);
            a.pid = Some(me);
            a.process_started_at = Some(Utc::now() - chrono::Duration::hours(24 * 30));
            a
        };
        let real = matching("claude-code-4242");
        let backend = Mock::with(vec![
            Response::Agent {
                agent: impostor_by_name,
            },
            Response::Agents {
                agents: vec![real.clone()],
            },
        ]);
        assert_eq!(
            found_by_pid(&backend, &input, Some(me))
                .await
                .unwrap()
                .unwrap()
                .id,
            real.id,
            "the name answered, but the process behind it did not match"
        );

        // Unverifiable ancestry authorises nothing. A birth time nobody
        // can read leaves nothing to check a record against, and this
        // answer is used to release leases and deregister — so a record
        // that answers to the right NAME is still refused, and no
        // listing is even asked for.
        let live_but_unverifiable = agent(&session_name(&input.session_id), true);
        for pid in [None, Some(u32::MAX)] {
            let backend = Mock::with(vec![Response::Agent {
                agent: live_but_unverifiable.clone(),
            }]);
            assert!(
                found_by_pid(&backend, &input, pid).await.unwrap().is_none(),
                "a name is not proof when nothing can confirm the process"
            );
            assert_eq!(backend.requests().len(), 1, "and nothing is listed");
        }

        // A project spans its main checkout and every linked worktree,
        // so narrowing the listing to the project is not the same as
        // being in the same tree.
        // Owned, not a shared path under the system temp directory:
        // tests run in parallel and a fixed name is a fixture two of
        // them can fight over.
        let other_tree = tempfile::tempdir().unwrap();
        let elsewhere = {
            let mut a = matching("claude-in-another-worktree");
            // A real directory, so this tests the comparison rather than
            // a path that fails to resolve for an unrelated reason.
            a.spec.workdir = Some(other_tree.path().to_path_buf());
            a
        };
        let backend = Mock::with(vec![
            Response::error(agentdocker_core::ErrorCode::NotFound, "no such agent"),
            Response::Agents {
                agents: vec![elsewhere],
            },
        ]);
        assert!(
            found_by_pid(&backend, &input, Some(me))
                .await
                .unwrap()
                .is_none(),
            "another worktree of the same project is another place to work"
        );
        // And the same session under the other half's name, with our own
        // session id on it, is us.
        let mut ours_by_id = matching("claude-code-4242");
        ours_by_id
            .spec
            .labels
            .insert("session_id".to_owned(), input.session_id.clone());
        let backend = Mock::with(vec![
            Response::error(agentdocker_core::ErrorCode::NotFound, "no such agent"),
            Response::Agents {
                agents: vec![ours_by_id.clone()],
            },
        ]);
        assert_eq!(
            found_by_pid(&backend, &input, Some(me))
                .await
                .unwrap()
                .unwrap()
                .id,
            ours_by_id.id
        );
    }

    #[test]
    fn session_name_is_prefix_of_id() {
        assert_eq!(session_name("0123456789abcdef"), "claude-01234567");
        assert_eq!(session_name("abc"), "claude-abc");
    }

    #[test]
    fn edited_path_only_for_edit_tools_and_is_absolute() {
        let mut ev = input("PreToolUse");
        ev.tool_name = Some("Bash".into());
        ev.tool_input = Some(json!({ "command": "ls" }));
        assert!(edited_path(&ev).is_none());

        ev.tool_name = Some("Write".into());
        ev.tool_input = Some(json!({ "file_path": "brand-new.rs", "content": "" }));
        let path = edited_path(&ev).unwrap();
        assert!(path.is_absolute());
        assert!(path.ends_with("brand-new.rs"));

        ev.tool_name = Some("NotebookEdit".into());
        ev.tool_input = Some(json!({ "notebook_path": "/tmp/nb.ipynb" }));
        assert_eq!(
            edited_path(&ev).unwrap(),
            normalize(Path::new("/tmp/nb.ipynb"))
        );
    }

    #[tokio::test]
    async fn pre_read_observes_and_stale_edit_is_denied_before_claiming() {
        let backend = Mock::with(vec![
            Response::Agent {
                agent: agent("reader", true),
            },
            Response::Reads { reads: vec![] },
        ]);
        let mut event = input("PreToolUse");
        event.tool_name = Some("Read".into());
        event.tool_input = Some(json!({"file_path": "file.rs"}));
        assert!(
            claude_code(&backend, &event, &opts())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            matches!(&backend.requests()[1], Request::Observe { paths, .. } if paths.len() == 1)
        );
        event.tool_name = Some("Edit".into());
        for _ in 0..2 {
            let backend = Mock::with(vec![
                Response::Agent {
                    agent: agent("reader", true),
                },
                Response::Stale {
                    stale: vec![agentdocker_core::StalePath {
                        path: "file.rs".into(),
                        observed: "old".into(),
                        current: Some("new".into()),
                        reason: "changed".into(),
                    }],
                },
            ]);
            let output = claude_code(&backend, &event, &opts())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(output["hookSpecificOutput"]["permissionDecision"], "deny");
            assert!(
                !backend
                    .requests()
                    .iter()
                    .any(|r| matches!(r, Request::Claim { .. }))
            );
        }
    }

    #[tokio::test]
    async fn pre_tool_use_denies_on_conflict() {
        let me = agent("claude-01234567", true);
        let backend = Mock::with(vec![
            Response::Agent { agent: me.clone() },
            Response::Stale { stale: vec![] },
            Response::Error {
                code: ErrorCode::Conflict,
                message: "held by reviewer".into(),
                details: Some(json!({ "held_by": [{ "note": "refactoring" }] })),
            },
        ]);
        let mut ev = input("PreToolUse");
        ev.tool_name = Some("Edit".into());
        ev.tool_input = Some(json!({ "file_path": "/tmp/shared.rs" }));

        let out = claude_code(&backend, &ev, &opts()).await.unwrap().unwrap();
        let specific = &out["hookSpecificOutput"];
        assert_eq!(specific["permissionDecision"], "deny");
        let reason = specific["permissionDecisionReason"].as_str().unwrap();
        assert!(reason.contains("held by reviewer"));
        assert!(reason.contains("refactoring"));

        let requests = backend.requests();
        assert!(matches!(
            &requests[2],
            Request::Claim { agent, resource, ttl_secs: 600, .. }
                if agent == me.id.as_str() && resource.starts_with("path:/")
        ));
    }

    #[tokio::test]
    async fn pre_tool_use_is_silent_when_claim_succeeds() {
        let backend = Mock::with(vec![
            Response::Agent {
                agent: agent("claude-01234567", true),
            },
            Response::Stale { stale: vec![] },
            Response::Ok, // stands in for Response::Lease
        ]);
        let mut ev = input("PreToolUse");
        ev.tool_name = Some("MultiEdit".into());
        ev.tool_input = Some(json!({ "file_path": "/tmp/x.rs" }));
        assert!(claude_code(&backend, &ev, &opts()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn session_start_registers_and_orients() {
        let other = agent("reviewer", true);
        let me = agent("claude-01234567", true);
        let backend = Mock::with(vec![
            Response::error(ErrorCode::NotFound, "no agent"),
            Response::Agent { agent: me.clone() },
            Response::Agents {
                agents: vec![other.clone(), me.clone(), agent("old", false)],
            },
            Response::Messages {
                messages: vec![message(other.id.as_str(), "hi there")],
            },
        ]);
        let out = claude_code(&backend, &input("SessionStart"), &opts())
            .await
            .unwrap()
            .unwrap();
        let text = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(text.contains("agent `claude-01234567`"));
        assert!(text.contains("reviewer (claude-code)"));
        assert!(!text.contains("old ("));
        assert!(text.contains("reviewer [chat]: hi there"));

        let requests = backend.requests();
        assert!(matches!(
            &requests[1],
            Request::Register { spec, .. } if spec.name == "claude-01234567" && spec.runtime == RUNTIME
        ));
    }

    #[tokio::test]
    async fn session_start_names_project_mates_before_strangers() {
        use agentdocker_core::ProjectRef;
        let mut me = agent("claude-01234567", true);
        me.project = Some(ProjectRef::directory("/work/alpha"));
        let mut mate = agent("mate", true);
        mate.project = Some(ProjectRef::directory("/work/alpha"));
        let mut stranger = agent("stranger", true);
        stranger.project = Some(ProjectRef::directory("/work/beta"));
        let backend = Mock::with(vec![
            Response::Agent { agent: me.clone() },
            Response::Agents {
                agents: vec![stranger, me.clone(), mate, agent("nowhere", true)],
            },
            Response::Messages {
                messages: Vec::new(),
            },
        ]);
        let out = claude_code(&backend, &input("SessionStart"), &opts())
            .await
            .unwrap()
            .unwrap();
        let text = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(text.contains("in project `alpha`"), "{text}");
        assert!(text.contains("In this project: mate ("), "{text}");
        assert!(text.contains("Elsewhere: stranger ("), "{text}");
        assert!(text.contains("nowhere ("), "{text}");
        assert!(text.find("mate (").unwrap() < text.find("stranger (").unwrap());
        assert!(text.contains("`--to project`"), "{text}");
    }

    #[tokio::test]
    async fn post_tool_use_reports_the_checkout_when_in_a_repository() {
        let git_ok = std::process::Command::new("git")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !git_ok {
            return;
        }
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args([
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "init.defaultBranch=main",
                ])
                .args(args)
                .env("HOME", dir.path())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        assert!(git(&["init", "-q"]));
        assert!(git(&["commit", "-q", "--allow-empty", "-m", "root"]));

        let mut me = agent("claude-01234567", true);
        me.spec.workdir = Some(repo.clone());
        let backend = Mock::with(vec![
            Response::Agent { agent: me.clone() },
            Response::Messages { messages: vec![] },
        ]);
        let mut ev = input("PostToolUse");
        ev.cwd = Some(repo.clone());
        assert!(claude_code(&backend, &ev, &opts()).await.unwrap().is_none());
        let reported = backend
            .requests()
            .into_iter()
            .find_map(|r| match r {
                Request::Report { agent, vcs } => Some((agent, vcs)),
                _ => None,
            })
            .expect("a report was sent");
        assert_eq!(reported.0, me.id.as_str());
        assert_eq!(reported.1.unwrap().branch.as_deref(), Some("main"));

        // Outside a repository nothing is reported. The fixture record and
        // hook still describe the same verified physical checkout.
        me.spec.workdir = Some(std::env::temp_dir());
        let quiet = Mock::with(vec![
            Response::Agent { agent: me.clone() },
            Response::Messages { messages: vec![] },
        ]);
        claude_code(&quiet, &input("PostToolUse"), &opts())
            .await
            .unwrap();
        assert!(
            !quiet
                .requests()
                .iter()
                .any(|r| matches!(r, Request::Report { .. }))
        );
    }

    #[tokio::test]
    async fn post_tool_use_only_speaks_when_messages_exist() {
        let me = agent("claude-01234567", true);
        let quiet = Mock::with(vec![
            Response::Agent { agent: me.clone() },
            Response::Messages { messages: vec![] },
        ]);
        assert!(
            claude_code(&quiet, &input("PostToolUse"), &opts())
                .await
                .unwrap()
                .is_none()
        );

        let busy = Mock::with(vec![
            Response::Agent { agent: me.clone() },
            Response::Messages {
                messages: vec![message("someone", "ping")],
            },
            Response::Agents { agents: vec![] },
        ]);
        let out = claude_code(&busy, &input("PostToolUse"), &opts())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        assert!(
            out["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
                .contains("ping")
        );
    }

    #[tokio::test]
    async fn stop_releases_and_blocks_when_messages_wait() {
        let me = agent("claude-01234567", true);
        let backend = Mock::with(vec![
            Response::Agent { agent: me.clone() },
            Response::Leases { leases: vec![] },
            Response::Messages {
                messages: vec![message("someone", "please review PR 7")],
            },
            Response::Agents { agents: vec![] },
        ]);
        let out = claude_code(&backend, &input("Stop"), &opts())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out["decision"], "block");
        assert!(
            out["reason"]
                .as_str()
                .unwrap()
                .contains("please review PR 7")
        );
        assert!(matches!(
            &backend.requests()[1],
            Request::ReleaseAll { agent, .. } if agent == me.id.as_str()
        ));
    }

    #[tokio::test]
    async fn stop_never_blocks_twice_or_when_asked_not_to() {
        let me = agent("claude-01234567", true);
        let backend = Mock::with(vec![
            Response::Agent { agent: me.clone() },
            Response::Leases { leases: vec![] },
        ]);
        let mut ev = input("Stop");
        ev.stop_hook_active = true;
        assert!(claude_code(&backend, &ev, &opts()).await.unwrap().is_none());
        // Inbox was never consulted.
        assert_eq!(backend.requests().len(), 2);

        let backend = Mock::with(vec![
            Response::Agent { agent: me.clone() },
            Response::Leases { leases: vec![] },
        ]);
        let quiet = ClaudeCodeArgs {
            no_wake: true,
            ..opts()
        };
        assert!(
            claude_code(&backend, &input("Stop"), &quiet)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn session_end_releases_and_deregisters() {
        let me = agent("claude-01234567", true);
        let backend = Mock::with(vec![
            Response::Agent { agent: me.clone() },
            Response::Leases { leases: vec![] },
            Response::Agent { agent: me.clone() },
        ]);
        assert!(
            claude_code(&backend, &input("SessionEnd"), &opts())
                .await
                .unwrap()
                .is_none()
        );
        let requests = backend.requests();
        assert!(matches!(&requests[1], Request::ReleaseAll { .. }));
        assert!(matches!(&requests[2], Request::Deregister { agent } if agent == me.id.as_str()));
    }

    #[tokio::test]
    async fn unknown_agent_at_session_end_is_a_no_op() {
        // Two lookups, not one: the name this adapter would have chosen,
        // then the pid, because the MCP half may own the name. Both
        // missing still means there is nothing to end.
        let backend = Mock::with(vec![
            Response::error(ErrorCode::NotFound, "nope"),
            Response::Agents { agents: vec![] },
        ]);
        assert!(
            claude_code(&backend, &input("SessionEnd"), &opts())
                .await
                .unwrap()
                .is_none()
        );
        // Two lookups where the pid is knowable, one where it is not:
        // `host_pid` walks real ancestry and is allowed to decline, and a
        // test that assumed either would be flaky on the machine that
        // disagreed.
        assert_eq!(
            backend.requests().len(),
            if host_pid().is_some() { 2 } else { 1 }
        );
    }

    #[test]
    fn hook_inventory_requires_the_complete_installed_adapter_and_preserves_mentions() {
        use agentdocker_core::runtime::{Wiring, spec};
        use agentdocker_host::runtimes::{claude_hook_command, hooks_wiring};
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("space ' and $()/agentdocker");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        let command = claude_hook_command(&bin).unwrap();
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", &command])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"hook\nclaude-code\n");

        let mention =
            json!({"hooks":[{"type":"command", "command":"echo 'agentdocker hook claude-code'"}]});
        let mut settings = json!({"hooks":{"Stop":[mention.clone()]}});
        assert_eq!(merge_claude_code_hooks(&mut settings, &command).unwrap(), 6);
        assert_eq!(settings["hooks"]["Stop"][0], mention);
        let file = tmp.path().join(".claude/settings.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let runtime = spec("claude-code").unwrap();
        for (change, expected) in [
            (None, Wiring::Wired),
            (Some("matcher"), Wiring::Missing),
            (Some("event"), Wiring::Missing),
        ] {
            let mut candidate = settings.clone();
            match change {
                Some("matcher") => candidate["hooks"]["PreToolUse"][0]["matcher"] = json!("Edit"),
                Some("event") => {
                    candidate["hooks"]
                        .as_object_mut()
                        .unwrap()
                        .remove("SessionEnd");
                }
                _ => {}
            }
            std::fs::write(&file, serde_json::to_vec(&candidate).unwrap()).unwrap();
            assert_eq!(hooks_wiring(runtime, tmp.path(), "agentdocker"), expected);
        }
    }

    #[test]
    fn codex_scope_repair_keeps_other_hooks_narrow_and_is_idempotent() {
        let own = json!({"type":"command", "command":"agentdocker hook codex"});
        let other = json!({"type":"command", "command":"user-check"});
        let mut settings =
            json!({"hooks":{"PreToolUse":[{"matcher":"Edit", "hooks":[own, other.clone()]}]}});
        assert_eq!(
            merge_hooks(&mut settings, "agentdocker hook codex", "codex").unwrap(),
            7
        );
        let entries = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(entries[0]["matcher"], "Edit");
        assert_eq!(entries[0]["hooks"], json!([other]));
        assert!(entries[1].get("matcher").is_none());
        let before = settings.clone();
        assert_eq!(
            merge_hooks(&mut settings, "agentdocker hook codex", "codex").unwrap(),
            0
        );
        assert_eq!(settings, before);
    }

    #[test]
    fn codex_interrupt_timeout_obeys_provider_limit_and_preserves_foreign_hooks() {
        let mut settings = json!({});
        merge_hooks(&mut settings, "agentdocker hook codex", "codex").unwrap();
        assert_eq!(settings["hooks"]["Interrupt"][0]["hooks"][0]["timeout"], 3);
        settings["hooks"]["Interrupt"][0]["hooks"][0]["timeout"] = json!(15);
        let foreign = json!({"type":"command", "command":"user-check", "timeout":9});
        settings["hooks"]["Interrupt"][0]["hooks"]
            .as_array_mut()
            .unwrap()
            .push(foreign.clone());
        assert_eq!(
            merge_hooks(&mut settings, "agentdocker hook codex", "codex").unwrap(),
            1
        );
        assert_eq!(settings["hooks"]["Interrupt"][0]["hooks"][0]["timeout"], 3);
        assert_eq!(settings["hooks"]["Interrupt"][0]["hooks"][1], foreign);
        assert_eq!(
            merge_hooks(&mut settings, "agentdocker hook codex", "codex").unwrap(),
            0
        );
    }

    #[test]
    fn installer_reports_matcher_upgrade_so_settings_are_written() {
        let mut settings = json!({});
        merge_claude_code_hooks(&mut settings, "agentdocker hook claude-code").unwrap();
        settings["hooks"]["PreToolUse"][0]["matcher"] = json!("Edit|Write|MultiEdit|NotebookEdit");
        assert_eq!(
            merge_claude_code_hooks(&mut settings, "agentdocker hook claude-code").unwrap(),
            1
        );
        assert_eq!(settings["hooks"]["PreToolUse"][0]["matcher"], EDIT_MATCHER);
        assert_eq!(
            merge_claude_code_hooks(&mut settings, "agentdocker hook claude-code").unwrap(),
            0
        );
    }

    #[test]
    fn installer_upgrades_mixed_hooks_without_widening_user_hooks() {
        let mut settings = json!({});
        let command = "agentdocker hook claude-code";
        merge_claude_code_hooks(&mut settings, command).unwrap();
        let own = settings["hooks"]["PreToolUse"][0]["hooks"][0].clone();
        let user = json!({"type": "command", "command": "echo user", "timeout": 9});
        let empty = json!({"matcher": "Bash", "hooks": []});
        let missing = json!({"matcher": "Bash"});
        settings["hooks"]["PreToolUse"] = json!([
            {"matcher": "Edit", "hooks": [own, user]}, empty, missing
        ]);
        assert_eq!(merge_claude_code_hooks(&mut settings, command).unwrap(), 1);
        assert_eq!(
            settings["hooks"]["PreToolUse"],
            json!([
                {"matcher": "Edit", "hooks": [user]}, empty, missing,
                {"matcher": EDIT_MATCHER, "hooks": [own]}
            ])
        );
        let upgraded = settings.clone();
        assert_eq!(merge_claude_code_hooks(&mut settings, command).unwrap(), 0);
        assert_eq!(settings, upgraded);
    }

    #[test]
    fn merge_is_idempotent_and_keeps_existing_hooks() {
        let mut settings = json!({
            "permissions": { "allow": ["Bash(ls)"] },
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo hi" }] }
                ]
            }
        });
        let added =
            merge_claude_code_hooks(&mut settings, "/usr/local/bin/agentdocker hook claude-code")
                .unwrap();
        assert_eq!(added, 6);
        assert_eq!(settings["permissions"]["allow"][0], "Bash(ls)");
        let pre = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2);
        assert_eq!(pre[0]["matcher"], "Bash");
        assert_eq!(pre[1]["matcher"], EDIT_MATCHER);
        assert!(
            settings["hooks"]["SessionStart"][0]
                .get("matcher")
                .is_none()
        );

        let again =
            merge_claude_code_hooks(&mut settings, "/elsewhere/agentdocker hook claude-code")
                .unwrap();
        assert_eq!(again, 0);
        assert_eq!(settings["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn full_output_pipe_respects_delivery_deadline() {
        use std::os::fd::{AsRawFd, FromRawFd};
        let mut descriptors = [0; 2];
        // SAFETY: successful pipe initializes two owned file descriptors.
        assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
        let _read = unsafe { std::fs::File::from_raw_fd(descriptors[0]) };
        let write = unsafe { std::fs::File::from_raw_fd(descriptors[1]) };
        let start = tokio::time::Instant::now();
        let result = write_output_before(
            write.as_raw_fd(),
            &vec![b'x'; 4 * 1024 * 1024],
            start + std::time::Duration::from_millis(30),
        );
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
    }

    #[tokio::test]
    async fn timeout_after_reading_inbox_preserves_messages() {
        struct Slow {
            queued: RefCell<Vec<Envelope>>,
            agent: AgentRecord,
        }
        impl Backend for Slow {
            async fn call(&self, request: Request) -> Result<Response> {
                match request {
                    Request::Inspect { .. } => Ok(Response::Agent {
                        agent: self.agent.clone(),
                    }),
                    Request::Inbox { drain, .. } => {
                        assert!(!drain, "hooks must not destructively read inboxes");
                        Ok(Response::Messages {
                            messages: self.queued.borrow().clone(),
                        })
                    }
                    Request::List { .. } => std::future::pending().await,
                    _ => panic!("unexpected request {request:?}"),
                }
            }
        }
        let checkout = tempfile::TempDir::new().unwrap();
        let mut me = agent("me", true);
        me.spec.workdir = Some(checkout.path().to_path_buf());
        let slow = Slow {
            agent: me,
            queued: RefCell::new(vec![message("peer", "keep this")]),
        };
        let delivery = HookDelivery {
            backend: &slow,
            pending: RefCell::new(Vec::new()),
        };
        let mut event = input("UserPromptSubmit");
        event.cwd = Some(checkout.path().to_path_buf());
        assert!(
            bounded_claude_code(&delivery, &event, &opts())
                .await
                .is_err()
        );
        assert_eq!(slow.queued.borrow().len(), 1);
        assert_eq!(delivery.pending.borrow().len(), 1);
    }

    #[tokio::test]
    async fn unresponsive_backend_cannot_exceed_hook_budget() {
        struct Never;
        impl Backend for Never {
            async fn call(&self, _: Request) -> Result<Response> {
                std::future::pending().await
            }
        }
        let input = HookInput {
            hook_event_name: "SessionStart".into(),
            session_id: "timeout".into(),
            ..HookInput::default()
        };
        let opts = ClaudeCodeArgs {
            ttl: 600,
            no_wake: false,
            digest_entries: 20,
            digest_chars: 2000,
            prompt_digest_entries: 5,
            prompt_digest_chars: 500,
        };
        let result = bounded_claude_code(&Never, &input, &opts).await;
        assert!(result.unwrap_err().to_string().contains("hook budget"));
    }
    #[test]
    fn transcript_tail_is_bounded_and_refuses_symlinks_and_special_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript");
        std::fs::write(&path, "line\n".repeat(TRANSCRIPT_TAIL as usize)).unwrap();
        assert!(transcript_tail(&path).unwrap().len() <= TRANSCRIPT_TAIL as usize);
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(transcript_tail(&link), None);
        let pipe = dir.path().join("pipe");
        let raw = std::ffi::CString::new(pipe.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: valid NUL-terminated test path.
        assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);
        assert_eq!(transcript_tail(&pipe), None);
        assert_eq!(transcript_tail(dir.path()), None);
    }

    #[tokio::test]
    async fn prompt_name_lookup_failure_precedes_cursor_advancement() {
        struct FailingNames(Mock);
        impl Backend for FailingNames {
            async fn call(&self, request: Request) -> Result<Response> {
                if matches!(request, Request::List { .. }) {
                    anyhow::bail!("names unavailable");
                }
                self.0.call(request).await
            }
        }
        let me = agent("claude-01234567", true);
        let backend = FailingNames(Mock::with(vec![
            Response::Agent { agent: me },
            Response::Messages {
                messages: vec![message("someone", "hello")],
            },
        ]));
        assert!(
            claude_code(&backend, &input("UserPromptSubmit"), &opts())
                .await
                .is_err()
        );
        assert!(
            !backend
                .0
                .requests()
                .iter()
                .any(|r| matches!(r, Request::Journal { .. }))
        );
    }
}
