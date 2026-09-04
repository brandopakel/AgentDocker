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
const EDIT_MATCHER: &str = "Edit|Write|MultiEdit|NotebookEdit";
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
            let output = match claude_code(&client, &input, &opts).await {
                Ok(output) => output,
                Err(err) => {
                    eprintln!("agentdocker hook ({}): {err:#}", input.hook_event_name);
                    None
                }
            };
            if let Some(output) = output {
                println!("{output}");
            }
            Ok(())
        }
    }
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
            Ok(Some(context_output(
                "SessionStart",
                orientation(&me, &agents, &inbox),
            )))
        }
        "UserPromptSubmit" | "PostToolUse" => {
            let me = ensure_registered(backend, input).await?;
            let inbox = drain_inbox(backend, &me).await?;
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
            let Some(path) = edited_path(input) else {
                return Ok(None);
            };
            let me = ensure_registered(backend, input).await?;
            let response = backend
                .call(Request::Claim {
                    agent: me.id.to_string(),
                    resource: format!("path:{}", path.display()),
                    mode: LeaseMode::Exclusive,
                    ttl_secs: opts.ttl,
                    note: Some(format!("editing in Claude Code session {}", me.spec.name)),
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

async fn all_agents<B: Backend>(backend: &B) -> Result<Vec<AgentRecord>> {
    match backend.call(Request::List { all: true }).await? {
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
    let others: Vec<String> = agents
        .iter()
        .filter(|a| a.id != me.id && a.status.is_live())
        .map(|a| {
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
        })
        .collect();
    let mut text = format!("AgentDocker: this session is agent `{}`. ", me.spec.name);
    if others.is_empty() {
        text.push_str("No other agents are live right now. ");
    } else {
        text.push_str(&format!("Other live agents: {}. ", others.join(", ")));
    }
    text.push_str(
        "Edits are leased automatically: if another agent holds a file, the edit is refused \
         with their name and note — coordinate instead of retrying. Talk to an agent with \
         `agentdocker send --to <name> \"<text>\"`; their replies are handed to you here as \
         they arrive. `agentdocker ps` and `agentdocker leases` show the current state.",
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
    async fn pre_tool_use_denies_on_conflict() {
        let me = agent("claude-01234567", true);
        let backend = Mock::with(vec![
            Response::Agent { agent: me.clone() },
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
            &requests[1],
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
            Request::ReleaseAll { agent } if agent == me.id.as_str()
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
}
