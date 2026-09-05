//! Claude Code hooks adapter.
//!
//! `agentdocker hook claude-code` is wired into Claude Code's hook events by
//! `agentdocker hook install claude-code`. Each invocation reads one event
//! as JSON on stdin and turns it into daemon calls:
//!
//! | event              | effect                                                                 |
//! |--------------------|------------------------------------------------------------------------|
//! | `SessionStart`     | register the session as an agent; tell the model who else is running and hand it queued messages |
//! | `UserPromptSubmit` | hand the model queued messages as context                              |
//! | `PreToolUse`       | claim `path:<file>` before Edit/Write/MultiEdit/NotebookEdit; deny the edit on conflict |
//! | `PostToolUse`      | hand the model queued messages as context                              |
//! | `Stop`             | release every lease; if messages are waiting, block the stop so the model handles them |
//! | `SessionEnd`       | release every lease and deregister                                     |
//!
//! The hook never breaks a session: if agentd is unreachable it prints a
//! note to stderr and exits 0, and Claude Code carries on as if the hook
//! were not there (an edit is allowed rather than denied).

use std::cell::RefCell;
use std::io::Read;
use std::os::unix::process::parent_id;
use std::path::{Path, PathBuf};

use agentdocker_core::{AgentRecord, AgentSpec, Envelope, ErrorCode, LeaseMode, Request, Response};
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::{Backend, Client};
use crate::format;

const RUNTIME: &str = "claude-code";
const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];
const EDIT_MATCHER: &str = "Edit|Write|MultiEdit|NotebookEdit|Read|Grep|Glob";
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
}

#[derive(Args, Debug)]
pub struct InstallArgs {
    #[arg(value_enum)]
    pub host: Host,
    /// Write to ~/.claude/settings.json instead of ./.claude/settings.json.
    #[arg(long)]
    pub user: bool,
}

#[derive(ValueEnum, Clone, Debug)]
pub enum Host {
    ClaudeCode,
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
}

pub async fn run(client: Client, args: HookArgs) -> Result<()> {
    // A hook that has to start the daemon may wait a moment, not stall
    // the editor: it fails open past this.
    let client = client.with_start_timeout(Some(std::time::Duration::from_secs(1)));
    match args.command {
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
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    serde_json::from_str(&raw).context("stdin is not a Claude Code hook event")
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
            Ok(Some(context_output(
                "SessionStart",
                orientation(&me, &agents, &inbox),
            )))
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
            if inbox.is_empty() {
                return Ok(None);
            }
            let agents = all_agents(backend).await?;
            let text = format!(
                "AgentDocker: {} new message(s) from other agents:\n{}",
                inbox.len(),
                messages_text(&inbox, &agents)
            );
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
            let Some(me) = current_agent(backend, input).await? else {
                return Ok(None);
            };
            release_all(backend, &me).await?;
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
            if let Some(me) = current_agent(backend, input).await? {
                release_all(backend, &me).await?;
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

async fn ensure_registered<B: Backend>(backend: &B, input: &HookInput) -> Result<AgentRecord> {
    if let Some(me) = current_agent(backend, input).await? {
        return Ok(me);
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

async fn release_all<B: Backend>(backend: &B, me: &AgentRecord) -> Result<()> {
    backend
        .call(Request::ReleaseAll {
            agent: me.id.to_string(),
            summary: None,
        })
        .await?;
    Ok(())
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

fn install_hooks(args: &InstallArgs) -> Result<()> {
    match args.host {
        Host::ClaudeCode => {}
    }
    let path = if args.user {
        std::env::home_dir()
            .context("cannot find home directory")?
            .join(".claude")
            .join("settings.json")
    } else {
        PathBuf::from(".claude").join("settings.json")
    };
    let mut settings: Value = if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("{} is not valid JSON", path.display()))?
    } else {
        json!({})
    };
    let exe = std::env::current_exe().context("cannot locate the agentdocker binary")?;
    let command = format!("{} hook claude-code", exe.display());
    let added = merge_claude_code_hooks(&mut settings, &command)?;
    if added == 0 {
        // Nothing to add, so leave the file byte-for-byte alone: a rewrite
        // would re-sort and re-indent the user's whole settings document.
        println!("{}: hooks already installed", path.display());
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(&settings)?),
    )
    .with_context(|| format!("cannot write {}", path.display()))?;
    println!(
        "{}: added {added} hook entries running `{command}`",
        path.display()
    );
    Ok(())
}

/// Add our hook entries to a Claude Code settings document. Entries whose
/// command already runs `hook claude-code` are left alone, so this is safe
/// to run repeatedly. Returns how many entries were added.
pub fn merge_claude_code_hooks(settings: &mut Value, command: &str) -> Result<usize> {
    const EVENTS: &[(&str, Option<&str>)] = &[
        ("SessionStart", None),
        ("UserPromptSubmit", None),
        ("PreToolUse", Some(EDIT_MATCHER)),
        ("PostToolUse", None),
        ("Stop", None),
        ("SessionEnd", None),
    ];
    let root = settings
        .as_object_mut()
        .context("settings must be a JSON object")?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("`hooks` must be a JSON object")?;
    let mut added = 0;
    for (event, matcher) in EVENTS {
        let entries = hooks
            .entry(*event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .with_context(|| format!("`hooks.{event}` must be an array"))?;
        let present = entries.iter().any(|entry| {
            entry["hooks"].as_array().is_some_and(|hooks| {
                hooks.iter().any(|hook| {
                    hook["command"]
                        .as_str()
                        .is_some_and(|c| c.contains("hook claude-code"))
                })
            })
        });
        if present {
            if *event == "PreToolUse" {
                for entry in entries.iter_mut() {
                    let ours = entry["hooks"].as_array().is_some_and(|hs| {
                        !hs.is_empty()
                            && hs.iter().all(|h| {
                                h["command"]
                                    .as_str()
                                    .is_some_and(|c| c.contains("hook claude-code"))
                            })
                    });
                    if ours && entry["matcher"] != json!(EDIT_MATCHER) {
                        entry["matcher"] = json!(EDIT_MATCHER);
                        added += 1;
                    }
                }
            }
            continue;
        }
        let mut entry = json!({
            "hooks": [{ "type": "command", "command": command, "timeout": 15 }]
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
        record
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
        }
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

        let me = agent("claude-01234567", true);
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

        // Outside a repository nothing is reported.
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
        let backend = Mock::with(vec![Response::error(ErrorCode::NotFound, "nope")]);
        assert!(
            claude_code(&backend, &input("SessionEnd"), &opts())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(backend.requests().len(), 1);
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
        }
        impl Backend for Slow {
            async fn call(&self, request: Request) -> Result<Response> {
                match request {
                    Request::Inspect { .. } => Ok(Response::Agent {
                        agent: agent("me", true),
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
        let slow = Slow {
            queued: RefCell::new(vec![message("peer", "keep this")]),
        };
        let delivery = HookDelivery {
            backend: &slow,
            pending: RefCell::new(Vec::new()),
        };
        let mut event = input("UserPromptSubmit");
        event.cwd = None;
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
        };
        let result = bounded_claude_code(&Never, &input, &opts).await;
        assert!(result.unwrap_err().to_string().contains("hook budget"));
    }
}
