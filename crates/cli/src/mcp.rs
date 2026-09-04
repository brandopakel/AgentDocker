//! MCP server over stdio, proxying to agentd.
//!
//! Any MCP-capable host — Claude Code, Codex, Cursor, Gemini CLI, a custom
//! agent — spawns `agentdocker mcp` and its model gets AgentDocker's
//! registry, messaging and leases as tools, with no bespoke integration.
//!
//! The protocol surface needed is small (initialize, ping, tools/list,
//! tools/call), so this is hand-rolled JSON-RPC over newline-delimited
//! stdio rather than a dependency on a full MCP SDK. Everything written to
//! stdout is protocol; diagnostics go to stderr.

use std::collections::BTreeMap;
use std::os::unix::process::parent_id;
use std::time::{Duration, Instant};

use agentdocker_core::{
    AgentSpec, ErrorCode, LeaseId, LeaseMode, MessageId, Request, Response,
    protocol::DEFAULT_LEASE_TTL_SECS,
};
use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::client::{Backend, Client};

const SUPPORTED_PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
const LATEST_PROTOCOL: &str = "2025-06-18";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

/// Upper bound on `wait_for_messages`: the server handles one request at a
/// time, so a long wait blocks every other method for its duration.
const MAX_MESSAGE_WAIT_SECS: u64 = 300;
/// Upper bound on `claim` waits, matching the daemon's own limit.
const MAX_CLAIM_WAIT_SECS: u64 = 600;
/// How long `wait_for_messages` sleeps between inbox polls.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Args, Debug, Clone)]
pub struct McpArgs {
    /// Name to register this agent under (default: <runtime>-<host pid>).
    /// Ignored when AGENTDOCKER_AGENT_ID is set, since the agent already exists.
    #[arg(long)]
    pub name: Option<String>,
    /// Runtime of the host that spawned us: claude-code, codex, cursor, gemini-cli...
    #[arg(long, default_value = "mcp")]
    pub runtime: String,
    #[arg(long)]
    pub provider: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    /// Pid to register for liveness checks (default: our parent, the MCP host).
    #[arg(long)]
    pub pid: Option<u32>,
}

/// Who this MCP session is, from agentd's point of view.
#[derive(Debug, Clone)]
pub struct Identity {
    pub id: String,
    pub name: String,
    /// We registered the agent ourselves, so we deregister it on exit.
    pub registered_here: bool,
}

pub struct McpServer<B> {
    backend: B,
    identity: Identity,
}

/// Run the server on stdin/stdout until the host closes stdin.
pub async fn serve(client: Client, args: McpArgs) -> Result<()> {
    let identity = establish_identity(&client, &args).await?;
    eprintln!(
        "agentdocker mcp: serving as {} ({})",
        identity.name, identity.id
    );
    let server = McpServer::new(client, identity);
    // Whatever ends the session — stdin closing, or the host going away and
    // breaking the pipe — the agent we registered must be deregistered.
    let outcome = pump(&server).await;
    server.shutdown().await;
    outcome
}

/// Read requests until stdin closes or an I/O error ends the session.
async fn pump<B: Backend>(server: &McpServer<B>) -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let incoming: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(err) => {
                let response =
                    error_response(Value::Null, PARSE_ERROR, &format!("parse error: {err}"));
                write_line(&mut stdout, &response).await?;
                continue;
            }
        };
        if let Some(response) = server.handle_incoming(incoming).await {
            write_line(&mut stdout, &response).await?;
        }
    }
    Ok(())
}

async fn write_line(stdout: &mut tokio::io::Stdout, value: &Value) -> Result<()> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    stdout.write_all(line.as_bytes()).await?;
    stdout.flush().await?;
    Ok(())
}

/// Reuse the identity of the agent that spawned us, or register a new one
/// on behalf of the MCP host.
async fn establish_identity(client: &Client, args: &McpArgs) -> Result<Identity> {
    if let Some(id) = std::env::var("AGENTDOCKER_AGENT_ID")
        .ok()
        .filter(|id| !id.is_empty())
    {
        return match client.call(&Request::Inspect { agent: id.clone() }).await {
            Ok(Response::Agent { agent }) => Ok(Identity {
                id: agent.id.to_string(),
                name: agent.spec.name,
                registered_here: false,
            }),
            Ok(other) => bail!("unexpected reply to inspect: {other:?}"),
            Err(err) => Err(err.context(format!(
                "AGENTDOCKER_AGENT_ID={id} is set but agentd does not know that agent"
            ))),
        };
    }

    let host_pid = args.pid.unwrap_or_else(parent_id);
    let name = args
        .name
        .clone()
        .unwrap_or_else(|| format!("{}-{host_pid}", args.runtime));
    let workdir = std::env::current_dir()
        .ok()
        .map(|dir| dir.canonicalize().unwrap_or(dir));
    let spec = AgentSpec {
        name,
        runtime: args.runtime.clone(),
        provider: args.provider.clone(),
        model: args.model.clone(),
        command: Vec::new(),
        workdir,
        env: BTreeMap::new(),
        labels: BTreeMap::from([("via".to_owned(), "mcp".to_owned())]),
    };
    match client
        .call(&Request::Register {
            spec,
            pid: Some(host_pid),
        })
        .await
        .context("failed to register with agentd")?
    {
        Response::Agent { agent } => Ok(Identity {
            id: agent.id.to_string(),
            name: agent.spec.name,
            registered_here: true,
        }),
        other => bail!("unexpected reply to register: {other:?}"),
    }
}

impl<B: Backend> McpServer<B> {
    pub fn new(backend: B, identity: Identity) -> Self {
        Self { backend, identity }
    }

    /// Deregister if we were the ones who registered.
    pub async fn shutdown(&self) {
        if self.identity.registered_here {
            let _ = self
                .backend
                .call(Request::Deregister {
                    agent: self.identity.id.clone(),
                })
                .await;
        }
    }

    /// Handle one message or, for `2025-03-26` clients, a batch: a batch's
    /// replies go back as one array with notifications (which get no reply)
    /// left out, and an empty batch is invalid per JSON-RPC.
    pub async fn handle_incoming(&self, incoming: Value) -> Option<Value> {
        match incoming {
            Value::Array(items) => {
                if items.is_empty() {
                    return Some(error_response(Value::Null, INVALID_REQUEST, "empty batch"));
                }
                let mut replies = Vec::with_capacity(items.len());
                for item in items {
                    if let Some(reply) = self.handle(item).await {
                        replies.push(reply);
                    }
                }
                (!replies.is_empty()).then_some(Value::Array(replies))
            }
            single => self.handle(single).await,
        }
    }

    /// Handle one JSON-RPC message. Notifications produce no response.
    pub async fn handle(&self, message: Value) -> Option<Value> {
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            // Either a response to a request we never sent, or garbage.
            return id.map(|id| error_response(id, INVALID_REQUEST, "missing method"));
        };
        let id = id?;

        let result = match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tool_definitions() })),
            "tools/call" => self.call_tool(params).await,
            other => Err((METHOD_NOT_FOUND, format!("method not found: {other}"))),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => error_response(id, code, &message),
        })
    }

    fn initialize(&self, params: &Value) -> Value {
        let requested = params.get("protocolVersion").and_then(Value::as_str);
        let version = requested
            .filter(|v| SUPPORTED_PROTOCOLS.contains(v))
            .unwrap_or(LATEST_PROTOCOL);
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "agentdocker", "version": env!("CARGO_PKG_VERSION") },
            "instructions": format!(
                "You are agent `{}` (id {}) in AgentDocker, a coordination layer shared by \
                 every AI agent on this machine. Other agents may be editing the same files \
                 or working on the same tasks. Before editing a shared file or directory, \
                 call `claim` on `path:<absolute path>` and stop if it reports a conflict — \
                 the response says who holds it and why. Call `release` when done. Use \
                 `read_inbox` to see messages other agents sent you and `send_message` to \
                 reply, hand off work, or announce what you are doing. `list_agents` shows \
                 who else is running.",
                self.identity.name, self.identity.id
            ),
        })
    }

    async fn call_tool(&self, params: Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| (INVALID_PARAMS, "tools/call needs a name".to_owned()))?;
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        self.tool(name, arguments).await
    }

    async fn tool(&self, name: &str, arguments: Value) -> Result<Value, (i64, String)> {
        let me = self.identity.id.clone();
        match name {
            "whoami" => self.forward(Request::Inspect { agent: me }).await,
            "list_agents" => {
                let args: ListAgentsArgs = parse(arguments)?;
                self.forward(Request::List { all: args.all }).await
            }
            "inspect_agent" => {
                let args: InspectAgentArgs = parse(arguments)?;
                self.forward(Request::Inspect { agent: args.agent }).await
            }
            "send_message" => {
                let args: SendMessageArgs = parse(arguments)?;
                let payload = match (args.payload, args.text) {
                    (Some(payload), _) => payload,
                    (None, Some(text)) => json!({ "text": text }),
                    (None, None) => {
                        return Err((INVALID_PARAMS, "send_message needs text or payload".into()));
                    }
                };
                self.forward(Request::Send {
                    from: me,
                    to: args.to,
                    kind: args.kind,
                    payload,
                    reply_to: args.reply_to.map(MessageId::from),
                })
                .await
            }
            "read_inbox" => {
                let args: ReadInboxArgs = parse(arguments)?;
                self.forward(Request::Inbox {
                    agent: me,
                    drain: args.drain,
                })
                .await
            }
            "wait_for_messages" => {
                let args: WaitArgs = parse(arguments)?;
                self.wait_for_messages(Duration::from_secs(
                    args.timeout_secs.min(MAX_MESSAGE_WAIT_SECS),
                ))
                .await
            }
            "claim" => {
                let args: ClaimArgs = parse(arguments)?;
                let response = self
                    .backend
                    .call(Request::Claim {
                        agent: me,
                        resource: crate::resource_key(&args.resource),
                        mode: args.mode,
                        ttl_secs: args.ttl_secs,
                        note: args.note,
                        wait_secs: args.wait_secs.min(MAX_CLAIM_WAIT_SECS),
                    })
                    .await
                    .map_err(transport)?;
                Ok(match response {
                    // A conflict is an answer, not a failure: the model needs
                    // to read who holds the resource and decide what to do.
                    Response::Error {
                        code: ErrorCode::Conflict,
                        message,
                        details,
                    } => text_result(
                        &json!({
                            "claimed": false,
                            "conflict": message,
                            "held_by": details.and_then(|d| d.get("held_by").cloned()),
                        }),
                        false,
                    ),
                    Response::Lease { lease } => {
                        text_result(&json!({ "claimed": true, "lease": lease }), false)
                    }
                    other => render(other),
                })
            }
            "renew" => {
                let args: RenewArgs = parse(arguments)?;
                self.forward(Request::Renew {
                    agent: me,
                    lease: LeaseId::from(args.lease.as_str()),
                    ttl_secs: args.ttl_secs,
                })
                .await
            }
            "release" => {
                let args: ReleaseArgs = parse(arguments)?;
                self.forward(Request::Release {
                    agent: me,
                    lease: LeaseId::from(args.lease.as_str()),
                })
                .await
            }
            "list_leases" => {
                let args: ListLeasesArgs = parse(arguments)?;
                self.forward(Request::Leases {
                    agent: args.agent,
                    resource: args.resource.as_deref().map(crate::resource_key),
                })
                .await
            }
            other => Err((INVALID_PARAMS, format!("unknown tool: {other}"))),
        }
    }

    async fn forward(&self, request: Request) -> Result<Value, (i64, String)> {
        let response = self.backend.call(request).await.map_err(transport)?;
        Ok(render(response))
    }

    /// Poll the inbox until something arrives or the timeout passes. Polling
    /// (rather than a live subscription) means a message can never fall in
    /// the gap between "stopped listening" and "connection closed".
    async fn wait_for_messages(&self, timeout: Duration) -> Result<Value, (i64, String)> {
        let started = Instant::now();
        loop {
            let response = self
                .backend
                .call(Request::Inbox {
                    agent: self.identity.id.clone(),
                    drain: true,
                })
                .await
                .map_err(transport)?;
            match response {
                Response::Messages { messages } if messages.is_empty() => {}
                other => return Ok(render(other)),
            }
            if started.elapsed() >= timeout {
                return Ok(text_result(
                    &json!({ "messages": [], "timed_out": true }),
                    false,
                ));
            }
            tokio::time::sleep(POLL_INTERVAL.min(timeout.saturating_sub(started.elapsed()))).await;
        }
    }
}

// ----- tool argument shapes -------------------------------------------------

#[derive(Deserialize, Default)]
struct ListAgentsArgs {
    #[serde(default)]
    all: bool,
}

#[derive(Deserialize)]
struct InspectAgentArgs {
    agent: String,
}

#[derive(Deserialize)]
struct SendMessageArgs {
    to: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    payload: Option<Value>,
    #[serde(default = "default_kind")]
    kind: String,
    #[serde(default)]
    reply_to: Option<String>,
}

#[derive(Deserialize)]
struct ReadInboxArgs {
    #[serde(default = "default_true")]
    drain: bool,
}

#[derive(Deserialize)]
struct WaitArgs {
    #[serde(default = "default_wait")]
    timeout_secs: u64,
}

#[derive(Deserialize)]
struct ClaimArgs {
    resource: String,
    #[serde(default)]
    mode: LeaseMode,
    #[serde(default = "default_ttl")]
    ttl_secs: u64,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    wait_secs: u64,
}

#[derive(Deserialize)]
struct RenewArgs {
    lease: String,
    #[serde(default = "default_ttl")]
    ttl_secs: u64,
}

#[derive(Deserialize)]
struct ReleaseArgs {
    lease: String,
}

#[derive(Deserialize, Default)]
struct ListLeasesArgs {
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    resource: Option<String>,
}

fn default_kind() -> String {
    "chat".to_owned()
}

fn default_true() -> bool {
    true
}

fn default_wait() -> u64 {
    30
}

fn default_ttl() -> u64 {
    DEFAULT_LEASE_TTL_SECS
}

// ----- helpers --------------------------------------------------------------

fn parse<T: for<'de> Deserialize<'de>>(arguments: Value) -> Result<T, (i64, String)> {
    serde_json::from_value(arguments)
        .map_err(|err| (INVALID_PARAMS, format!("invalid arguments: {err}")))
}

fn transport(err: anyhow::Error) -> (i64, String) {
    (INTERNAL_ERROR, format!("agentd unreachable: {err:#}"))
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

/// Wrap a value as the text content of a tool result.
fn text_result(value: &Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

/// Turn a daemon response into a tool result, unwrapping the payload so the
/// model sees the data rather than the protocol envelope.
fn render(response: Response) -> Value {
    match response {
        Response::Error {
            code,
            message,
            details,
        } => text_result(
            &json!({ "error": message, "code": code, "details": details }),
            true,
        ),
        Response::Agent { agent } => text_result(&json!(agent), false),
        Response::Agents { agents } => text_result(&json!({ "agents": agents }), false),
        Response::Sent {
            message,
            subscribers,
        } => text_result(
            &json!({ "sent": true, "message_id": message, "live_subscribers": subscribers }),
            false,
        ),
        Response::Messages { messages } => text_result(&json!({ "messages": messages }), false),
        Response::Lease { lease } => text_result(&json!(lease), false),
        Response::Leases { leases } => text_result(&json!({ "leases": leases }), false),
        Response::Ok => text_result(&json!({ "ok": true }), false),
        other => text_result(&json!(other), false),
    }
}

fn tool_definitions() -> Vec<Value> {
    let resource_doc = "Resource key `kind:value`, e.g. `path:/abs/file`, `path:/abs/dir` \
                        (covers everything beneath), `branch:name`, `task:ID`.";
    vec![
        json!({
            "name": "whoami",
            "description": "This agent's own record in AgentDocker: id, name, runtime, status.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        json!({
            "name": "list_agents",
            "description": "List the agents AgentDocker knows about on this host. Live ones by default.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "all": { "type": "boolean", "description": "Include agents that have exited.", "default": false }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "inspect_agent",
            "description": "Everything known about one agent, by id, id prefix, or name.",
            "inputSchema": {
                "type": "object",
                "properties": { "agent": { "type": "string" } },
                "required": ["agent"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "send_message",
            "description": "Send a message to another agent (by id or name), to a topic (`topic:name`), or to everyone (`all`). Give `text`, or a structured `payload` object.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "Agent id/name, `topic:<name>`, or `all`." },
                    "text": { "type": "string" },
                    "payload": { "type": "object", "description": "Structured payload instead of text." },
                    "kind": { "type": "string", "description": "chat, task, handoff, question, answer, notice...", "default": "chat" },
                    "reply_to": { "type": "string", "description": "Id of the message this answers." }
                },
                "required": ["to"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "read_inbox",
            "description": "Messages other agents sent this agent. Removes them from the inbox unless drain is false.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "drain": { "type": "boolean", "default": true }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "wait_for_messages",
            "description": "Block until at least one message arrives for this agent, or the timeout passes (at most 300 s; nothing else is served meanwhile). Returns and drains everything that arrived.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "timeout_secs": { "type": "integer", "minimum": 0, "maximum": MAX_MESSAGE_WAIT_SECS, "default": 30 }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "claim",
            "description": format!("Take a time-limited lease on a resource so no other agent works on it at the same time. {resource_doc} On conflict returns claimed=false and who holds it — do not proceed; message the holder or wait."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "resource": { "type": "string", "description": resource_doc },
                    "mode": { "type": "string", "enum": ["exclusive", "shared"], "default": "exclusive" },
                    "ttl_secs": { "type": "integer", "minimum": 1, "default": DEFAULT_LEASE_TTL_SECS, "description": "Seconds until the lease expires unless renewed." },
                    "note": { "type": "string", "description": "What you are doing with it, shown to agents that conflict." },
                    "wait_secs": { "type": "integer", "minimum": 0, "maximum": MAX_CLAIM_WAIT_SECS, "default": 0, "description": "Wait this long for the resource to free up before reporting a conflict." }
                },
                "required": ["resource"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "renew",
            "description": "Extend a lease this agent holds.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "lease": { "type": "string", "description": "Lease id from claim." },
                    "ttl_secs": { "type": "integer", "minimum": 1, "default": DEFAULT_LEASE_TTL_SECS }
                },
                "required": ["lease"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "release",
            "description": "Release a lease this agent holds. Do this as soon as you are done with the resource.",
            "inputSchema": {
                "type": "object",
                "properties": { "lease": { "type": "string", "description": "Lease id from claim." } },
                "required": ["lease"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "list_leases",
            "description": "Current leases, optionally filtered by holder or by overlap with a resource.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "agent": { "type": "string", "description": "Only leases held by this agent." },
                    "resource": { "type": "string", "description": format!("Only leases overlapping this resource. {resource_doc}") }
                },
                "additionalProperties": false
            }
        }),
    ]
}

#[cfg(test)]
mod tests {
    use agentdocker_core::AgentId;
    use chrono::Utc;

    use super::*;

    use crate::client::mock::Mock;

    fn server(responses: Vec<Response>) -> McpServer<Mock> {
        McpServer::new(
            Mock::with(responses),
            Identity {
                id: "abc123".into(),
                name: "tester".into(),
                registered_here: true,
            },
        )
    }

    fn rpc(id: u64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    fn tool_text(result: &Value) -> Value {
        let text = result["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap()
    }

    #[tokio::test]
    async fn initialize_negotiates_protocol_version() {
        let s = server(vec![]);
        let reply = s
            .handle(rpc(
                1,
                "initialize",
                json!({ "protocolVersion": "2025-03-26" }),
            ))
            .await
            .unwrap();
        assert_eq!(reply["result"]["protocolVersion"], "2025-03-26");
        assert!(
            reply["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains("tester")
        );

        let reply = s
            .handle(rpc(
                2,
                "initialize",
                json!({ "protocolVersion": "1999-01-01" }),
            ))
            .await
            .unwrap();
        assert_eq!(reply["result"]["protocolVersion"], LATEST_PROTOCOL);
    }

    #[tokio::test]
    async fn notifications_get_no_reply_and_unknown_methods_error() {
        let s = server(vec![]);
        let none = s
            .handle(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await;
        assert!(none.is_none());

        let reply = s.handle(rpc(3, "resources/list", json!({}))).await.unwrap();
        assert_eq!(reply["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(reply["id"], 3);
    }

    #[tokio::test]
    async fn tools_list_has_schemas() {
        let s = server(vec![]);
        let reply = s.handle(rpc(4, "tools/list", json!({}))).await.unwrap();
        let tools = reply["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "whoami",
                "list_agents",
                "inspect_agent",
                "send_message",
                "read_inbox",
                "wait_for_messages",
                "claim",
                "renew",
                "release",
                "list_leases"
            ]
        );
        assert!(tools.iter().all(|t| t["inputSchema"]["type"] == "object"));
    }

    #[tokio::test]
    async fn batches_get_one_array_reply_without_notifications() {
        let s = server(vec![]);
        let reply = s
            .handle_incoming(json!([
                rpc(1, "ping", json!({})),
                { "jsonrpc": "2.0", "method": "notifications/initialized" },
                rpc(2, "tools/list", json!({})),
            ]))
            .await
            .unwrap();
        let replies = reply.as_array().expect("batch reply is an array");
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0]["id"], 1);
        assert_eq!(replies[1]["id"], 2);

        let only_notifications = s
            .handle_incoming(json!([{ "jsonrpc": "2.0", "method": "notifications/x" }]))
            .await;
        assert!(only_notifications.is_none());

        let empty = s.handle_incoming(json!([])).await.unwrap();
        assert_eq!(empty["error"]["code"], INVALID_REQUEST);

        let single = s.handle_incoming(rpc(3, "ping", json!({}))).await.unwrap();
        assert!(single.is_object());
        assert_eq!(single["id"], 3);
    }

    #[tokio::test]
    async fn path_resources_are_canonicalised_like_the_cli() {
        let s = server(vec![Response::Ok, Response::Leases { leases: vec![] }]);
        // `src` exists relative to the test's working directory (the crate root).
        s.handle(rpc(
            20,
            "tools/call",
            json!({ "name": "claim", "arguments": { "resource": "path:src" } }),
        ))
        .await;
        s.handle(rpc(
            21,
            "tools/call",
            json!({ "name": "list_leases", "arguments": { "resource": "task:T-1" } }),
        ))
        .await;
        let requests = s.backend.requests.lock().unwrap();
        assert!(matches!(
            &requests[0],
            Request::Claim { resource, .. } if resource.starts_with("path:/") && resource.ends_with("/src")
        ));
        assert!(matches!(
            &requests[1],
            Request::Leases { resource: Some(resource), .. } if resource == "task:T-1"
        ));
    }

    #[tokio::test]
    async fn wait_timeout_is_bounded_in_the_schema() {
        let s = server(vec![]);
        let reply = s.handle(rpc(30, "tools/list", json!({}))).await.unwrap();
        let wait = reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "wait_for_messages")
            .unwrap();
        assert_eq!(
            wait["inputSchema"]["properties"]["timeout_secs"]["maximum"],
            MAX_MESSAGE_WAIT_SECS
        );
    }

    #[tokio::test]
    async fn claim_wait_is_bounded_at_the_daemon_limit() {
        let s = server(vec![]);
        let reply = s.handle(rpc(31, "tools/list", json!({}))).await.unwrap();
        let claim = reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "claim")
            .unwrap();
        assert_eq!(
            claim["inputSchema"]["properties"]["wait_secs"]["maximum"],
            MAX_CLAIM_WAIT_SECS
        );
    }

    #[tokio::test]
    async fn send_message_uses_own_identity_and_wraps_text() {
        let s = server(vec![Response::Sent {
            message: MessageId::from("m1".to_owned()),
            subscribers: 1,
        }]);
        let reply = s
            .handle(rpc(
                5,
                "tools/call",
                json!({ "name": "send_message", "arguments": { "to": "reviewer", "text": "hi" } }),
            ))
            .await
            .unwrap();
        assert_eq!(reply["result"]["isError"], false);
        assert_eq!(tool_text(&reply)["message_id"], "m1");

        let requests = s.backend.requests.lock().unwrap();
        assert_eq!(
            requests[0],
            Request::Send {
                from: "abc123".into(),
                to: "reviewer".into(),
                kind: "chat".into(),
                payload: json!({ "text": "hi" }),
                reply_to: None,
            }
        );
    }

    #[tokio::test]
    async fn send_message_without_content_is_invalid_params() {
        let s = server(vec![]);
        let reply = s
            .handle(rpc(
                6,
                "tools/call",
                json!({ "name": "send_message", "arguments": { "to": "x" } }),
            ))
            .await
            .unwrap();
        assert_eq!(reply["error"]["code"], INVALID_PARAMS);
    }

    #[tokio::test]
    async fn claim_conflict_is_an_answer_not_an_error() {
        let s = server(vec![Response::Error {
            code: ErrorCode::Conflict,
            message: "held by someone".into(),
            details: Some(json!({ "held_by": [{ "holder": "other" }] })),
        }]);
        let reply = s
            .handle(rpc(
                7,
                "tools/call",
                json!({ "name": "claim", "arguments": { "resource": "path:/x" } }),
            ))
            .await
            .unwrap();
        assert_eq!(reply["result"]["isError"], false);
        let body = tool_text(&reply);
        assert_eq!(body["claimed"], false);
        assert_eq!(body["held_by"][0]["holder"], "other");

        let requests = s.backend.requests.lock().unwrap();
        assert!(matches!(
            &requests[0],
            Request::Claim { agent, resource, mode: LeaseMode::Exclusive, ttl_secs, .. }
                if agent == "abc123" && resource == "path:/x" && *ttl_secs == DEFAULT_LEASE_TTL_SECS
        ));
    }

    #[tokio::test]
    async fn daemon_errors_become_tool_errors() {
        let s = server(vec![Response::error(ErrorCode::NotFound, "no such lease")]);
        let reply = s
            .handle(rpc(
                8,
                "tools/call",
                json!({ "name": "release", "arguments": { "lease": "nope" } }),
            ))
            .await
            .unwrap();
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(tool_text(&reply)["error"], "no such lease");
    }

    #[tokio::test]
    async fn read_inbox_drains_by_default() {
        let s = server(vec![Response::Messages { messages: vec![] }]);
        s.handle(rpc(9, "tools/call", json!({ "name": "read_inbox" })))
            .await
            .unwrap();
        let requests = s.backend.requests.lock().unwrap();
        assert_eq!(
            requests[0],
            Request::Inbox {
                agent: "abc123".into(),
                drain: true
            }
        );
    }

    #[tokio::test]
    async fn wait_for_messages_polls_until_something_arrives() {
        let message = agentdocker_core::Envelope::new(
            "other",
            agentdocker_core::Destination::Agent(AgentId::from("abc123")),
            "chat",
            json!({ "text": "now" }),
            None,
            Utc::now(),
        );
        let s = server(vec![
            Response::Messages { messages: vec![] },
            Response::Messages { messages: vec![] },
            Response::Messages {
                messages: vec![message],
            },
        ]);
        let reply = s
            .handle(rpc(
                10,
                "tools/call",
                json!({ "name": "wait_for_messages", "arguments": { "timeout_secs": 5 } }),
            ))
            .await
            .unwrap();
        assert_eq!(tool_text(&reply)["messages"][0]["payload"]["text"], "now");
        assert_eq!(s.backend.requests.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn wait_for_messages_times_out_cleanly() {
        let s = server(vec![]);
        // Mock returns Response::Ok once responses run out; make it empty inboxes.
        *s.backend.responses.lock().unwrap() =
            std::iter::repeat_n(Response::Messages { messages: vec![] }, 10).collect();
        let reply = s
            .handle(rpc(
                11,
                "tools/call",
                json!({ "name": "wait_for_messages", "arguments": { "timeout_secs": 0 } }),
            ))
            .await
            .unwrap();
        assert_eq!(tool_text(&reply)["timed_out"], true);
    }

    #[tokio::test]
    async fn shutdown_deregisters_only_if_registered_here() {
        let s = server(vec![]);
        s.shutdown().await;
        assert!(matches!(
            s.backend.requests.lock().unwrap().as_slice(),
            [Request::Deregister { agent }] if agent == "abc123"
        ));

        let adopted = McpServer::new(
            Mock::default(),
            Identity {
                id: "abc123".into(),
                name: "tester".into(),
                registered_here: false,
            },
        );
        adopted.shutdown().await;
        assert!(adopted.backend.requests.lock().unwrap().is_empty());
    }
}
