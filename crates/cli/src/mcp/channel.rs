//! Opt-in Claude channel input over the existing private daemon queue.
//! A completed stdout write is only an offer. The durable envelope remains
//! until a model ACK or verified provider transcript confirms actual input.
use super::{Backend, Identity, McpServer, error_response};
use agentdocker_core::input::channel::{Ownership, TokenEvidence};
use agentdocker_core::{MessageId, Request, Response};
use agentdocker_host::{dirs, lock};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    future::{Future, poll_fn},
    pin::Pin,
    task::Poll,
    time::Duration,
};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

mod stdio;

/// Serve the channel as the instance `adapter` names (its token).
pub(super) async fn serve(server: &McpServer<impl Backend>, adapter: &str) -> Result<()> {
    let (input, output) = stdio::open()?;
    pump(server, input, output, adapter).await
}

const FRAME_BYTES: usize = 1024 * 1024;
const ACTIVE_CALLS: usize = 8;
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_EVERY: Duration = Duration::from_millis(250);
const RECEIPT_TIMEOUT: Duration = Duration::from_secs(30);
// Claude installs its channel handler after the MCP initialize exchange has
// completed. A retained head offered on the first immediate polling tick can
// be dropped during that startup window. Keep control/receipt requests live
// while giving the provider a bounded settling interval before the first offer.
// This is a startup mitigation, not a provider receipt or permission signal.
const FIRST_OFFER_SETTLE: Duration = Duration::from_secs(1);

/// An adapter that owns its session: it holds its provider process's lock
/// and its agent's, and each says which instance holds it.
pub(crate) struct Owner {
    _process: lock::Lock,
    _agent: lock::Lock,
    /// This instance's token, written into both lock files and into its
    /// receipt-missing pause.
    pub(super) token: String,
}

/// What an adapter's start found. Only `Failed` may be reported: it holds
/// the provider generation's process lock, so no other adapter of that
/// generation serves the session and its report cannot overwrite one's.
pub(crate) enum Start {
    Owned(Owner),
    /// Another adapter of this provider process holds its lock (found
    /// under the gates, so not a probe): a duplicate MCP entry.
    Duplicate,
    /// Ownership could not be established. Nothing is reported, for an
    /// owner may be serving behind the fault.
    Unknown(anyhow::Error),
    /// It holds the generation's process lock and the gates and failed
    /// after taking them; `held` keeps them until the failure is reported.
    Failed {
        cause: anyhow::Error,
        held: Exclusion,
    },
}

/// The gates and the process lock a failed start keeps through its report.
pub(crate) struct Exclusion {
    process: Option<lock::Lock>,
    gates: Vec<lock::Lock>,
    /// Run between letting the process lock go and letting the gates go.
    #[cfg(test)]
    between: Option<Box<dyn FnOnce() + Send>>,
}

impl Exclusion {
    fn new(gates: Vec<lock::Lock>, process: lock::Lock) -> Self {
        Self {
            process: Some(process),
            gates,
            #[cfg(test)]
            between: None,
        }
    }
}

impl Drop for Exclusion {
    /// The process lock first, the gates after, whether the report was made
    /// or its task was cancelled: a successor waiting on the gates finds the
    /// failed start's process lock already free, and never takes it for a
    /// serving owner.
    fn drop(&mut self) {
        drop(self.process.take());
        #[cfg(test)]
        if let Some(between) = self.between.take() {
            between();
        }
        self.gates.clear();
    }
}

/// How many delivery gates there are. A key falls on one of them by its
/// hash, so the folder holds a bounded number of gate files however many
/// sessions come and go.
const GATES: usize = 64;
/// How long an adapter waits for a gate before it says it is waiting.
const GATE_PATIENCE: Duration = Duration::from_secs(10);

pub(super) async fn acquire(identity: &Identity) -> Start {
    acquire_for(&dirs::home(), identity).await
}

fn process_key(pid: u32, started: chrono::DateTime<chrono::Utc>) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "process-{pid}-{:x}",
        Sha256::digest(started.to_rfc3339().as_bytes())
    )
}

/// Whether `id` can name an owner lock or gate.
fn nameable(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn directory(home: &std::path::Path) -> Result<std::path::PathBuf> {
    let directory = home.join("channel-input");
    dirs::ensure_private_dir(&directory)?;
    Ok(directory)
}

fn gate_path(directory: &std::path::Path, key: &str) -> std::path::PathBuf {
    use sha2::{Digest, Sha256};
    let stripe = usize::from(Sha256::digest(key.as_bytes())[0]) % GATES;
    directory.join(format!("stripe-{stripe:02}.gate"))
}

/// Take the gates `keys` fall on, in one order for everyone, so two takers
/// never wait on each other in a cycle. With a `deadline`, `Ok(None)` when
/// it passes first; without one, wait as long as a holder holds, saying so
/// once after [`GATE_PATIENCE`]: a holder is a hook or a probe, each
/// bounded by its own deadline, and its lock ends with its process.
async fn take_gates(
    directory: &std::path::Path,
    keys: &[&str],
    deadline: Option<tokio::time::Instant>,
) -> Result<Option<Vec<lock::Lock>>> {
    let mut paths: Vec<_> = keys.iter().map(|key| gate_path(directory, key)).collect();
    paths.sort();
    paths.dedup();
    let started = tokio::time::Instant::now();
    let mut said = false;
    let mut held = Vec::new();
    for path in paths {
        dirs::private_file(&path, true, false)
            .with_context(|| format!("cannot open the delivery gate {}", path.display()))?;
        let mut pause = Duration::from_millis(1);
        loop {
            match lock::try_exclusive_existing(&path)
                .with_context(|| format!("cannot take the delivery gate {}", path.display()))?
            {
                Some(lock) => {
                    held.push(lock);
                    break;
                }
                None => {
                    let now = tokio::time::Instant::now();
                    if deadline.is_some_and(|deadline| now >= deadline) {
                        return Ok(None);
                    }
                    if deadline.is_none() && !said && now - started >= GATE_PATIENCE {
                        eprintln!(
                            "agentdocker channel: waiting for the delivery gate {} that a hook or probe holds",
                            path.display()
                        );
                        said = true;
                    }
                    let wake = deadline.map_or(now + pause, |deadline| deadline.min(now + pause));
                    tokio::time::sleep_until(wake).await;
                    pause = (pause * 2).min(Duration::from_millis(20));
                }
            }
        }
    }
    Ok(Some(held))
}

/// Take an owner lock without waiting: `None` when another holds it.
fn take_owner(directory: &std::path::Path, key: &str) -> std::io::Result<Option<lock::Lock>> {
    let path = directory.join(format!("{key}.lock"));
    dirs::private_file(&path, true, false)?;
    lock::try_exclusive_existing(&path)
}

/// Start under the gates: the process lock, then the agent lock, then the
/// token in both, and only then let the gates go. A would-block on the
/// process lock under the gates is a real owner of this provider process.
async fn acquire_for(home: &std::path::Path, identity: &Identity) -> Start {
    let (Some(pid), Some(started)) = (identity.host_pid, identity.host_started_at) else {
        return Start::Unknown(anyhow::anyhow!(
            "channel provider PID or generation is unavailable"
        ));
    };
    let process_key = process_key(pid, started);
    let directory = match directory(home) {
        Ok(directory) => directory,
        Err(error) => return Start::Unknown(error.context("channel ownership folder unusable")),
    };
    let agent_key = nameable(&identity.id).then_some(identity.id.as_str());
    let keys: Vec<&str> = std::iter::once(process_key.as_str())
        .chain(agent_key)
        .collect();
    let gates = match take_gates(&directory, &keys, None).await {
        Ok(Some(gates)) => gates,
        Ok(None) => unreachable!("a wait without a deadline does not run out"),
        Err(error) => return Start::Unknown(error),
    };
    let process = match take_owner(&directory, &process_key) {
        Ok(Some(lock)) => lock,
        Ok(None) => return Start::Duplicate,
        Err(error) => {
            return Start::Unknown(
                anyhow::Error::new(error)
                    .context("cannot open this provider process's channel lock"),
            );
        }
    };
    let failed = |cause: anyhow::Error, gates, process| Start::Failed {
        cause,
        held: Exclusion::new(gates, process),
    };
    let Some(agent_key) = agent_key else {
        return failed(
            anyhow::anyhow!("invalid agent ID for channel ownership"),
            gates,
            process,
        );
    };
    let agent = match take_owner(&directory, agent_key) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            return failed(
                anyhow::anyhow!(
                    "another channel adapter serves this agent from another provider process; close the duplicate MCP entry"
                ),
                gates,
                process,
            );
        }
        Err(error) => {
            return failed(
                anyhow::Error::new(error).context("cannot open this agent's channel lock"),
                gates,
                process,
            );
        }
    };
    let token = uuid::Uuid::new_v4().simple().to_string();
    let line = format!("{token}\n");
    if let Err(error) = process
        .write_contents(line.as_bytes())
        .and_then(|()| agent.write_contents(line.as_bytes()))
    {
        drop(agent);
        return failed(
            anyhow::Error::new(error).context("cannot name this adapter in its channel locks"),
            gates,
            process,
        );
    }
    drop(gates);
    Start::Owned(Owner {
        _process: process,
        _agent: agent,
        token,
    })
}

/// A start as `identity` under `home`, for tests outside this module.
#[cfg(test)]
pub(crate) async fn start_for(home: &std::path::Path, identity: &Identity) -> Start {
    acquire_for(home, identity).await
}

/// What a probe of a session's channel ownership found, holding the gates
/// it took until it is dropped: a hook keeps it through reading, writing
/// and acknowledging the queue, so no adapter can begin to own the session
/// meanwhile.
pub(crate) struct Probe {
    pub(crate) ownership: Ownership,
    /// Why ownership is unknown, when it is.
    pub(crate) why: Option<String>,
    _gates: Vec<lock::Lock>,
}

impl Probe {
    fn unknown(why: impl std::fmt::Display) -> Self {
        Self {
            ownership: Ownership::Unknown,
            why: Some(why.to_string()),
            _gates: Vec::new(),
        }
    }
}

/// Probe `agent`'s channel ownership under the gates its provider process
/// and agent ID fall on, waiting for them until `deadline`. An owner lock
/// free under the gates is no owner; a held one is an owner, whose file
/// says which instance it is; anything unreadable is not knowing.
pub(crate) async fn probe(
    home: &std::path::Path,
    agent: &agentdocker_core::AgentRecord,
    deadline: tokio::time::Instant,
) -> Probe {
    let process_key = match (agent.pid, agent.process_started_at) {
        (Some(pid), Some(started)) => Some(process_key(pid, started)),
        _ => None,
    };
    let agent_key = nameable(agent.id.as_str()).then_some(agent.id.as_str());
    let keys: Vec<&str> = process_key
        .as_deref()
        .into_iter()
        .chain(agent_key)
        .collect();
    if keys.is_empty() {
        return Probe::unknown("the session names no provider process or agent to probe");
    }
    let directory = match directory(home) {
        Ok(directory) => directory,
        Err(error) => return Probe::unknown(format!("{error:#}")),
    };
    let gates = match take_gates(&directory, &keys, Some(deadline)).await {
        Ok(Some(gates)) => gates,
        Ok(None) => return Probe::unknown("the delivery gate stayed held"),
        Err(error) => return Probe::unknown(format!("{error:#}")),
    };
    let mut held = None;
    for key in &keys {
        let path = directory.join(format!("{key}.lock"));
        match lock::try_exclusive_existing(&path) {
            Ok(Some(free)) => drop(free),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(None) => {
                held.get_or_insert(path);
            }
            Err(error) => {
                return Probe::unknown(format!("cannot probe {}: {error}", path.display()));
            }
        }
    }
    let ownership = match held {
        None => Ownership::Absent,
        Some(path) => match read_token(&path) {
            Ok(evidence) => Ownership::Held(evidence),
            Err(error) => {
                return Probe::unknown(format!("cannot read {}: {error}", path.display()));
            }
        },
    };
    Probe {
        ownership,
        why: None,
        _gates: gates,
    }
}

/// The first bytes of a held owner lock file, as evidence of its holder.
#[cfg(unix)]
fn read_token(path: &std::path::Path) -> std::io::Result<TokenEvidence> {
    use std::io::Read;
    let mut bytes = Vec::new();
    dirs::open_private(path)?.take(64).read_to_end(&mut bytes)?;
    Ok(TokenEvidence::read(&bytes))
}

/// A held byte-range lock refuses other readers on Windows, so a holder's
/// token cannot be read there and its ownership is not known.
#[cfg(windows)]
fn read_token(_path: &std::path::Path) -> std::io::Result<TokenEvidence> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "a held channel lock cannot be read on Windows",
    ))
}

/// Whether an adapter holds `agent`'s session: what receipt recovery asks
/// before it reads a transcript for a channel's receipts.
pub(crate) async fn held(
    home: &std::path::Path,
    agent: &agentdocker_core::AgentRecord,
    deadline: tokio::time::Instant,
) -> bool {
    matches!(
        probe(home, agent, deadline).await.ownership,
        Ownership::Held(_)
    )
}

type Pending<'a> = Pin<Box<dyn Future<Output = Option<Value>> + 'a>>;

/// The channel transport keeps receipt calls responsive even while another
/// tool waits for a human answer. All stdout frames use this single writer.
async fn pump<B: Backend, R: AsyncBufRead + Unpin, W: stdio::Output>(
    server: &McpServer<B>,
    mut input: R,
    mut output: W,
    adapter: &str,
) -> Result<()> {
    let mut pending: Vec<Pending<'_>> = Vec::new();
    let mut frame = Vec::new();
    let mut initialized = false;
    // Input delivery is bound — readiness reported — once the handshake
    // is done and, for a session that asked to resume an earlier one,
    // once its hooks have said which session runs (or the wait expired).
    let mut ready = false;
    let mut first_offer_at = None;
    let mut vouch_deadline: Option<tokio::time::Instant> = None;
    let mut vouch_tick = tokio::time::interval(POLL_EVERY);
    vouch_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // One look at the daemon's record at a time, polled beside the
    // transport so a slow daemon never holds up control or receipts.
    let mut look: Option<Pin<Box<dyn Future<Output = Option<String>> + '_>>> = None;
    let mut tick = tokio::time::interval(POLL_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // The head on offer: its id, when it was last written, whether the
    // missing receipt has been said, and whether it has been repeated.
    let mut offered: Option<(MessageId, tokio::time::Instant, bool, bool)> = None;
    // Whether any offer has been received this session: the proof that
    // Claude's channel handler is there. Until it is, the first offer may
    // be the one written before the handler was registered.
    let mut receipt_seen = false;
    let mut unavailable = false;
    let mut last_report = tokio::time::Instant::now();
    let mut readiness_unavailable = false;
    let mut provider_blocked = None;
    loop {
        tokio::select! {
            incoming = read_frame(&mut input, &mut frame) => {
                let Some(bytes) = incoming? else { return Ok(()) };
                if bytes.iter().all(u8::is_ascii_whitespace) { continue; }
                let value: Value = match serde_json::from_slice(&bytes) {
                    Ok(value) => value,
                    Err(error) => {
                        write(server, &mut output, &error_response(Value::Null, super::PARSE_ERROR, &format!("parse error: {error}"))).await?;
                        continue;
                    }
                };
                if value["method"] == "notifications/initialized" && value.get("id").is_none() {
                    if !initialized {
                        match &server.resume_vouch {
                            None => {
                                crate::input_status::report(&server.backend, &server.identity.id, server.identity.host_started_at,
                                    agentdocker_core::InputReport::Ready).await?;
                                ready = true;
                                first_offer_at = Some(tokio::time::Instant::now() + FIRST_OFFER_SETTLE);
                            }
                            // The handshake completes now; readiness waits
                            // for the hooks' word, below, or the deadline.
                            Some(vouch) => vouch_deadline = Some(tokio::time::Instant::now() + vouch.wait),
                        }
                    }
                    initialized = true;
                    last_report = tokio::time::Instant::now();
                    continue;
                }
                // A channel always reserves service for handshake, ping and
                // explicit receipts, even when all long-running slots are used.
                if priority(&value) {
                    let id = value.get("id").cloned().unwrap_or(Value::Null);
                    let receipt = carries_receipt(&value);
                    match tokio::time::timeout(IO_TIMEOUT, server.handle_incoming(value)).await {
                        Ok(Some(response)) => {
                            // A receipt the daemon accepted proves the
                            // handler is there, whichever offer it names:
                            // from here on no offer is repeated.
                            if receipt && receipt_accepted(&response) { receipt_seen = true; }
                            write(server, &mut output, &response).await?
                        }
                        Ok(None) => {},
                        Err(_) => write(server, &mut output, &error_response(id, super::INTERNAL_ERROR, "receipt/control request timed out; inspect the retained inbox before retrying")).await?,
                    }
                } else if pending.len() < ACTIVE_CALLS {
                    pending.push(Box::pin(server.handle_incoming(value)));
                } else if let Some(id) = value.get("id") {
                    write(server, &mut output, &error_response(id.clone(), -32000, "MCP request capacity is full; retry this request later")).await?;
                } else if value.is_array() {
                    // Reject every request in an over-capacity legacy batch.
                    let replies: Vec<_> = value.as_array().unwrap().iter().filter_map(|item| item.get("id")).map(|id|
                        error_response(id.clone(), -32000, "MCP request capacity is full; retry this request later")
                    ).collect();
                    if !replies.is_empty() { write(server, &mut output, &Value::Array(replies)).await?; }
                }
            }
            (index, response) = poll_fn(|cx| {
                for (index, future) in pending.iter_mut().enumerate() {
                    if let Poll::Ready(response) = future.as_mut().poll(cx) {
                        return Poll::Ready((index, response));
                    }
                }
                Poll::Pending
            }), if !pending.is_empty() => {
                drop(pending.swap_remove(index));
                if let Some(response) = response { write(server, &mut output, &response).await?; }
            }
            // Waiting for the hooks adapter to vouch for a resumed session:
            // the record this process is must carry a session id and be
            // this very process (pid and birth, both known). Which session
            // the hook names is accepted as it is, even when it differs
            // from what the command line asked for; a record for some
            // other or unknown process generation vouches for nothing.
            // Past the deadline, input is bound unvouched and the daemon's
            // own guard keeps an earlier record separate — said so, not
            // hidden.
            _ = vouch_tick.tick(), if initialized && !ready && look.is_none() => {
                // A look never outlives the wait: the last one is cut at the
                // deadline, so the bound is the ten seconds, not ten plus
                // one transport timeout.
                let remaining = vouch_deadline
                    .map(|deadline| deadline.saturating_duration_since(tokio::time::Instant::now()))
                    .unwrap_or(IO_TIMEOUT)
                    .min(IO_TIMEOUT)
                    .max(Duration::from_millis(1));
                look = Some(Box::pin(async move {
                    let record = tokio::time::timeout(remaining, server.backend.call(Request::Inspect {
                        agent: server.identity.id.clone(),
                    })).await;
                    match record {
                        Ok(Ok(Response::Agent { agent }))
                            if agent.pid.is_some()
                                && agent.pid == server.identity.host_pid
                                && agent.process_started_at.is_some()
                                && agent.process_started_at == server.identity.host_started_at =>
                        {
                            agent.spec.labels.get("session_id").filter(|id| !id.is_empty()).cloned()
                        }
                        _ => None,
                    }
                }));
            }
            vouched = poll_fn(|cx| match look.as_mut() {
                Some(pending) => pending.as_mut().poll(cx),
                None => Poll::Pending,
            }), if look.is_some() => {
                look = None;
                let vouch = server.resume_vouch.as_ref().expect("waiting only for a resume");
                let expired = vouch_deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline);
                if let Some(session) = vouched {
                    match &vouch.requested {
                        Some(requested) if requested != &session => eprintln!(
                            "agentdocker channel: hooks name session {session}, not the {requested} the command line asked for; following the hooks"
                        ),
                        _ => eprintln!("agentdocker channel: hooks vouched for session {session}; binding input"),
                    }
                } else if expired {
                    eprintln!(
                        "agentdocker channel: no hooks vouched for the resumed session within {}s; binding input unverified — an earlier record of this session stays separate",
                        vouch.wait.as_secs()
                    );
                } else {
                    continue;
                }
                crate::input_status::report(&server.backend, &server.identity.id, server.identity.host_started_at,
                    agentdocker_core::InputReport::Ready).await?;
                ready = true;
                first_offer_at = Some(tokio::time::Instant::now() + FIRST_OFFER_SETTLE);
                last_report = tokio::time::Instant::now();
            }
            _ = tick.tick(), if ready => {
                let reply = tokio::time::timeout(IO_TIMEOUT, server.backend.call(Request::DeliveryQueue {
                    agent: server.identity.id.clone(),
                })).await;
                let messages = match reply {
                    Ok(Ok(Response::Messages { messages })) => { unavailable = false; provider_blocked = None; messages },
                    Ok(Ok(Response::InputWaiting { availability, .. })) => {
                        if provider_blocked.as_ref() != Some(&availability) {
                            eprintln!("agentdocker channel: {}; messages remain queued", availability.issue.as_ref()
                                .map_or("provider unavailable", |issue| issue.kind.label()));
                            provider_blocked = Some(availability);
                        }
                        // Keep the offered ID: recovery must not replay input
                        // that may already have reached the provider.
                        continue;
                    }
                    _ => {
                        if !unavailable {
                            eprintln!("agentdocker channel: inbox unavailable; no delivery is confirmed and accepted messages remain queued");
                            unavailable = true;
                        }
                        continue;
                    }
                };
                let mut state_changed = false;
                if let Some((id, since, warned, repeated)) = &mut offered {
                    if let Some(message) = messages.iter().find(|message| &message.id == id) {
                        if !*repeated && !receipt_seen && since.elapsed() >= RECEIPT_TIMEOUT {
                            // The one offer a session can lose is its first,
                            // written before Claude registered its handler.
                            // Until a receipt has proved the handler is
                            // there, the head is offered a second time, once,
                            // under the same id; from then on the no-replay
                            // rule holds and a missing receipt pauses the
                            // queue visibly.
                            eprintln!("agentdocker channel: message {id} has no receipt after 30 seconds and no receipt has been seen yet this session; offering it once more");
                            write(server, &mut output, &offer(message, true)?).await?;
                            *repeated = true;
                            *since = tokio::time::Instant::now();
                        } else if !*warned && since.elapsed() >= RECEIPT_TIMEOUT {
                            eprintln!("agentdocker channel: message {id} has no receipt after 30 seconds; verify this session's channel opt-in and permissions");
                            *warned = true;
                            state_changed = true;
                        }
                    } else {
                        state_changed = *warned;
                        receipt_seen = true;
                        offered = None;
                    }
                }
                // Queue reachability is not evidence that the provider consumed
                // an offer. Keep a missing-receipt state visible until that ID
                // leaves the queue, including after failed diagnostic writes.
                if state_changed || last_report.elapsed() >= Duration::from_secs(30) {
                    let observation = match &offered {
                        Some((id, _, true, _)) => agentdocker_core::InputReport::Paused {
                            reason: agentdocker_core::input::receipt_missing_reason(id, adapter),
                        },
                        _ => agentdocker_core::InputReport::Ready,
                    };
                    let refreshed = crate::input_status::refresh_report(&server.backend, &server.identity.id,
                        server.identity.host_started_at, observation).await;
                    if !refreshed && !readiness_unavailable {
                        eprintln!("agentdocker channel: readiness refresh unavailable; current status will expire without changing the message queue");
                    }
                    readiness_unavailable = !refreshed;
                    last_report = tokio::time::Instant::now();
                }
                if offered.is_some() || first_offer_at.is_some_and(|at| tokio::time::Instant::now() < at) {
                    continue;
                }
                if let Some(message) = messages.first() {
                    write(server, &mut output, &offer(message, false)?).await?;
                    first_offer_at = None;
                    offered = Some((message.id.clone(), tokio::time::Instant::now(), false, false));
                }
            }
        }
    }
}

fn priority(value: &Value) -> bool {
    match value {
        Value::Array(requests) => !requests.is_empty() && requests.iter().all(priority_request),
        request => priority_request(request),
    }
}

fn priority_request(value: &Value) -> bool {
    matches!(value["method"].as_str(), Some("initialize" | "ping")) || is_receipt(value)
}

fn is_receipt(value: &Value) -> bool {
    value["method"] == "tools/call" && value["params"]["name"] == "acknowledge_messages"
}

/// Whether a control frame (one request or a legacy batch) carries a receipt.
fn carries_receipt(value: &Value) -> bool {
    match value {
        Value::Array(requests) => requests.iter().any(is_receipt),
        request => is_receipt(request),
    }
}

/// Whether a control reply records at least one receipt the daemon
/// accepted: a tool result that is neither a JSON-RPC error nor marked
/// `isError`. A refused or failed receipt proves nothing about the handler.
fn receipt_accepted(response: &Value) -> bool {
    let accepted = |reply: &Value| {
        reply.get("error").is_none()
            && reply.get("result").is_some()
            && reply["result"]["isError"] != true
            && reply["result"].get("content").is_some()
    };
    match response {
        Value::Array(replies) => replies.iter().any(accepted),
        reply => accepted(reply),
    }
}

#[cfg(test)]
mod priority_tests {
    use super::*;

    #[test]
    fn receipt_only_batches_bypass_busy_tools_but_mixed_or_invalid_batches_do_not() {
        let ack = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"acknowledge_messages"}});
        let ping = json!({"jsonrpc":"2.0","id":2,"method":"ping"});
        assert!(priority(&ack));
        assert!(priority(&json!([ack.clone(), ack.clone()])));
        assert!(priority(&json!([ack.clone(), ping])));
        assert!(!priority(
            &json!([ack.clone(),{"method":"tools/call","params":{"name":"ask_human"}}])
        ));
        for invalid in [json!([]), json!([null]), json!([[ack]])] {
            assert!(!priority(&invalid));
        }
    }

    #[test]
    fn only_an_accepted_receipt_counts_as_one_seen() {
        let ack = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"acknowledge_messages"}});
        let ping = json!({"jsonrpc":"2.0","id":2,"method":"ping"});
        assert!(carries_receipt(&ack));
        assert!(carries_receipt(&json!([ping.clone(), ack.clone()])));
        assert!(!carries_receipt(&ping));
        assert!(!carries_receipt(&json!([ping.clone()])));
        let accepted = json!({"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"{}"}],"isError":false}});
        let refused = json!({"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"no"}],"isError":true}});
        let error = json!({"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"invalid"}});
        let pong = json!({"jsonrpc":"2.0","id":2,"result":{}});
        assert!(receipt_accepted(&accepted));
        assert!(!receipt_accepted(&refused));
        assert!(!receipt_accepted(&error));
        assert!(!receipt_accepted(&pong), "a pong is not a receipt");
        assert!(receipt_accepted(&json!([pong.clone(), accepted])));
        assert!(!receipt_accepted(&json!([pong, refused, error])));
    }
}

/// The channel notification for one queued message. A repeat carries the
/// same `message_id` and says so, so the model acknowledges it once.
fn offer(message: &agentdocker_core::Envelope, repeat: bool) -> Result<Value> {
    let mut notification = json!({
        "jsonrpc": "2.0", "method": "notifications/claude/channel",
        "params": {"content": serde_json::to_string(&message.payload)?, "meta": {
            "message_id": message.id.as_str(), "from_agent": message.from,
            "kind": message.kind, "sent_at": message.sent_at.to_rfc3339(),
            "destination": serde_json::to_string(&message.to)?,
            "reply_destination": crate::format::reply_destination(message),
            "delivery_rule": "Acknowledge this message_id after receiving the full body. A repeated offer carries the same message_id and repeat=true: it is the same message, acknowledged once. Reply using send_message to reply_destination with reply_to=message_id so the response appears in the original app conversation. A human pause request requires stopping work and reporting that actual state there; a terminal-only answer is not an app reply.",
        }}
    });
    if let Some(question) = &message.reply_to {
        notification["params"]["meta"]["reply_to"] = json!(question.as_str());
    }
    // Claude Code takes `meta` as a map of strings, keys an identifier, and
    // drops a notification whose meta is anything else: a boolean here
    // lost every repeat.
    if repeat {
        notification["params"]["meta"]["repeat"] = json!("true");
    }
    Ok(notification)
}

#[cfg(test)]
mod meta_tests {
    use super::*;

    /// Claude Code's channel schema: `meta` is a map of strings, and each
    /// key matches `^[a-zA-Z_][a-zA-Z0-9_]*$`. A notification outside it is
    /// dropped without a word, so every offer is held to it.
    #[test]
    fn every_offer_s_meta_is_strings_under_identifier_keys() {
        let message = agentdocker_core::Envelope::new(
            "sender",
            agentdocker_core::Destination::Agent("receiver".into()),
            "chat",
            json!({"text": "hello"}),
            Some(agentdocker_core::MessageId::from("q1".to_owned())),
            chrono::Utc::now(),
        );
        for repeat in [false, true] {
            let offer = offer(&message, repeat).unwrap();
            let meta = offer["params"]["meta"].as_object().unwrap();
            for (key, value) in meta {
                let mut chars = key.chars();
                assert!(
                    chars
                        .next()
                        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_'),
                    "{key}"
                );
                assert!(value.is_string(), "{key} = {value}");
            }
            assert_eq!(meta.contains_key("repeat"), repeat);
        }
    }
}

#[cfg(test)]
mod emission_tests {
    use super::*;
    use agentdocker_core::usage::emission::{Channel, Emission};
    use std::cell::RefCell;
    use tokio::io::AsyncReadExt;

    struct Recorder(RefCell<Vec<Emission>>);
    impl Backend for Recorder {
        async fn call(&self, _: Request) -> Result<Response> {
            panic!("unexpected coordination request")
        }
        async fn record_emission(&self, _: &str, emission: Emission) {
            self.0.borrow_mut().push(emission);
        }
    }
    struct Broken;
    impl stdio::Output for Broken {
        async fn send(&mut self, _: &Value) -> Result<()> {
            bail!("fixture broken output")
        }
    }

    #[tokio::test]
    async fn emission_follows_a_complete_write_and_never_counts_failed_output() {
        let server = McpServer::new(
            Recorder(RefCell::new(Vec::new())),
            Identity {
                id: "receiver".into(),
                name: "fixture".into(),
                registered_here: false,
                host_pid: None,
                host_started_at: None,
            },
        );
        let value = json!({"jsonrpc":"2.0","method":"notifications/claude/channel","params":{"content":"é界🙂"}});
        let (output, mut input) = tokio::io::duplex(4096);
        let mut output = stdio::AsyncOutput(output);
        write(&server, &mut output, &value).await.unwrap();
        let mut expected = serde_json::to_vec(&value).unwrap();
        expected.push(b'\n');
        let mut received = vec![0; expected.len()];
        input.read_exact(&mut received).await.unwrap();
        assert_eq!(received, expected);
        assert!(write(&server, &mut Broken, &value).await.is_err());
        let records = server.backend.0.borrow();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].bytes, expected.len() as u64);
        assert_eq!(records[0].channel, Channel::Channel);
    }
}

async fn write<B: Backend>(
    server: &McpServer<B>,
    output: &mut impl stdio::Output,
    value: &Value,
) -> Result<()> {
    tokio::time::timeout(WRITE_TIMEOUT, output.send(value))
        .await
        .context("channel output stalled; queued messages remain unacknowledged")??;
    let channel = if value["method"] == "notifications/claude/channel" {
        agentdocker_core::usage::emission::Channel::Channel
    } else {
        agentdocker_core::usage::emission::Channel::Mcp
    };
    crate::emission::json_line(&server.backend, &server.identity.id, channel, value).await;
    Ok(())
}

/// Cancellation preserves the partial frame when a tool or polling tick wins
/// select. Reject oversized input before retaining an unbounded allocation.
async fn read_frame(
    input: &mut (impl AsyncBufRead + Unpin),
    frame: &mut Vec<u8>,
) -> Result<Option<Vec<u8>>> {
    loop {
        let bytes = input.fill_buf().await?;
        if bytes.is_empty() {
            return Ok((!frame.is_empty()).then(|| std::mem::take(frame)));
        }
        let end = bytes.iter().position(|byte| *byte == b'\n');
        let count = end.map_or(bytes.len(), |end| end + 1);
        if frame.len().saturating_add(count) > FRAME_BYTES {
            bail!("MCP input exceeds the 1 MiB frame limit");
        }
        frame.extend_from_slice(&bytes[..count]);
        input.consume(count);
        if end.is_some() {
            return Ok(Some(std::mem::take(frame)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::write_line;
    use super::*;

    /// The instance token the pump's tests serve as.
    const TEST_ADAPTER: &str = "0123456789abcdef0123456789abcdef";
    use agentdocker_core::{Destination, Envelope};
    use std::cell::RefCell;
    use tokio::io::{AsyncWriteExt, BufReader};

    fn identity(id: &str, pid: u32, born: chrono::DateTime<chrono::Utc>) -> Identity {
        Identity {
            id: id.into(),
            name: "fixture".into(),
            host_pid: Some(pid),
            host_started_at: Some(born),
            registered_here: false,
        }
    }

    fn record_of(identity: &Identity) -> agentdocker_core::AgentRecord {
        let mut record = agentdocker_core::AgentRecord::new(
            Default::default(),
            false,
            identity.host_started_at.unwrap(),
        );
        record.id = identity.id.as_str().into();
        record.pid = identity.host_pid;
        record.process_started_at = identity.host_started_at;
        record
    }

    fn soon() -> tokio::time::Instant {
        tokio::time::Instant::now() + Duration::from_secs(1)
    }

    /// One adapter owns a provider process; the next start of it is a
    /// duplicate; the hooks see the owner and which instance it is, and no
    /// owner once it is gone.
    #[tokio::test]
    async fn an_owner_is_exclusive_and_named_by_its_instance() {
        let home = tempfile::tempdir().unwrap();
        let born = chrono::Utc::now();
        let first = identity("receiver", 1234, born);
        let record = record_of(&first);
        assert_eq!(
            probe(home.path(), &record, soon()).await.ownership,
            Ownership::Absent
        );
        let Start::Owned(owner) = acquire_for(home.path(), &first).await else {
            panic!("the first start owns the session");
        };
        assert!(agentdocker_core::input::channel::is_token(&owner.token));
        assert!(matches!(
            acquire_for(home.path(), &first).await,
            Start::Duplicate
        ));
        assert_eq!(
            probe(home.path(), &record, soon()).await.ownership,
            Ownership::Held(TokenEvidence::Token(owner.token.clone()))
        );
        drop(owner);
        assert_eq!(
            probe(home.path(), &record, soon()).await.ownership,
            Ownership::Absent
        );
        // The folder holds the owner locks and a bounded set of gates.
        let gates = std::fs::read_dir(home.path().join("channel-input"))
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".gate")
            })
            .count();
        assert!((1..=GATES).contains(&gates), "{gates}");
    }

    /// The process lock outlives a change of agent ID, and a reused PID is
    /// another generation.
    #[tokio::test]
    async fn channel_ownership_survives_canonical_agent_id_changes() {
        let home = tempfile::tempdir().unwrap();
        let born = chrono::Utc::now();
        let first = identity("fresh", 1234, born);
        let Start::Owned(owner) = acquire_for(home.path(), &first).await else {
            panic!("owned");
        };
        let canonical = identity("canonical", 1234, born);
        assert!(
            matches!(acquire_for(home.path(), &canonical).await, Start::Duplicate),
            "a new agent ID cannot admit another channel for the same process"
        );
        let mut record = record_of(&canonical);
        assert!(
            held(home.path(), &record, soon()).await,
            "hooks find the process lock even after ID folding"
        );
        record.process_started_at = Some(born + chrono::Duration::seconds(1));
        assert!(
            !held(home.path(), &record, soon()).await,
            "a reused PID is another generation"
        );
        let next = identity("canonical", 1234, born + chrono::Duration::seconds(1));
        let Start::Owned(next_owner) = acquire_for(home.path(), &next).await else {
            panic!("another generation owns its own session");
        };
        drop(next_owner);
        drop(owner);
        assert!(matches!(
            acquire_for(home.path(), &canonical).await,
            Start::Owned(_)
        ));
        let unknown = Identity {
            host_started_at: None,
            ..canonical
        };
        assert!(matches!(
            acquire_for(home.path(), &unknown).await,
            Start::Unknown(_)
        ));
    }

    /// A probe holding the gates, however long, makes a starting adapter
    /// wait, never fail or take the probe for an owner; the hook's own wait
    /// ends at its deadline as not knowing.
    #[tokio::test(start_paused = true)]
    async fn a_held_gate_delays_a_start_and_is_never_an_owner() {
        let home = tempfile::tempdir().unwrap();
        let born = chrono::Utc::now();
        let starting = identity("receiver", 4321, born);
        let record = record_of(&starting);
        let hook = probe(home.path(), &record, soon()).await;
        assert_eq!(hook.ownership, Ownership::Absent);
        let start = tokio::spawn({
            let home = home.path().to_path_buf();
            let starting = starting.clone();
            async move { acquire_for(&home, &starting).await }
        });
        tokio::time::sleep(GATE_PATIENCE * 3).await;
        assert!(!start.is_finished(), "the start waits on the gate");
        // Another hook meanwhile cannot take the gate, and does not know.
        let other = probe(home.path(), &record, soon()).await;
        assert_eq!(other.ownership, Ownership::Unknown);
        assert!(other.why.is_some());
        drop(hook);
        assert!(matches!(start.await.unwrap(), Start::Owned(_)));
    }

    /// A start that fails after taking the generation's process lock keeps
    /// it, and the gates, until its failure is reported: a successor
    /// cannot start in between, and starts once it is let go.
    #[tokio::test(start_paused = true)]
    async fn a_failed_start_holds_its_exclusion_through_its_report() {
        let home = tempfile::tempdir().unwrap();
        let born = chrono::Utc::now();
        let invalid = identity("../not-an-id", 99, born);
        let Start::Failed { cause, held } = acquire_for(home.path(), &invalid).await else {
            panic!("an invalid agent ID fails under the process lock");
        };
        assert!(cause.to_string().contains("invalid agent ID"));
        let successor = tokio::spawn({
            let home = home.path().to_path_buf();
            async move { acquire_for(&home, &identity("receiver", 99, born)).await }
        });
        tokio::time::sleep(GATE_PATIENCE * 2).await;
        assert!(!successor.is_finished(), "held through the report");
        drop(held);
        assert!(matches!(successor.await.unwrap(), Start::Owned(_)));
    }

    /// A failed start lets its process lock go before its gates: at the
    /// moment between, the lock is free while the gates are still held, so
    /// a successor that was waiting on the gates never finds the lock held.
    #[tokio::test]
    async fn a_failed_start_lets_its_process_lock_go_before_its_gates() {
        let home = tempfile::tempdir().unwrap();
        let born = chrono::Utc::now();
        let invalid = identity("../not-an-id", 98, born);
        let Start::Failed { mut held, .. } = acquire_for(home.path(), &invalid).await else {
            panic!("failed under the process lock");
        };
        let directory = home.path().join("channel-input");
        let key = process_key(98, born);
        let gate = gate_path(&directory, &key);
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        held.between = Some(Box::new({
            let seen = seen.clone();
            move || {
                let process_free = take_owner(&directory, &key).unwrap().is_some();
                let gate_held = lock::try_exclusive_existing(&gate).unwrap().is_none();
                *seen.lock().unwrap() = Some((process_free, gate_held));
            }
        }));
        drop(held);
        assert_eq!(*seen.lock().unwrap(), Some((true, true)));
    }

    /// An owner file that fails its checks while an owner serves: the
    /// process lock's makes ownership unknown to a start and to a hook, and
    /// the agent lock's leaves the start a duplicate of the owner, so
    /// neither can report over the owner's word.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreadable_owner_file_is_not_knowing_and_reports_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let born = chrono::Utc::now();
        let serving = identity("receiver", 555, born);
        let Start::Owned(_owner) = acquire_for(home.path(), &serving).await else {
            panic!("owned");
        };
        let directory = home.path().join("channel-input");
        let process = directory.join(format!("{}.lock", process_key(555, born)));
        let agent = directory.join("receiver.lock");
        std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o622)).unwrap();
        assert!(matches!(
            acquire_for(home.path(), &serving).await,
            Start::Duplicate
        ));
        std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&process, std::fs::Permissions::from_mode(0o622)).unwrap();
        assert!(matches!(
            acquire_for(home.path(), &serving).await,
            Start::Unknown(_)
        ));
        let hook = probe(home.path(), &record_of(&serving), soon()).await;
        assert_eq!(hook.ownership, Ownership::Unknown);
    }

    /// The ownership folder replaced by a file: no start can establish
    /// ownership, and no hook can know it.
    #[tokio::test]
    async fn an_unusable_folder_is_not_knowing() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("channel-input"), b"not a folder").unwrap();
        let starting = identity("receiver", 77, chrono::Utc::now());
        assert!(matches!(
            acquire_for(home.path(), &starting).await,
            Start::Unknown(_)
        ));
        let hook = probe(home.path(), &record_of(&starting), soon()).await;
        assert_eq!(hook.ownership, Ownership::Unknown);
        assert!(hook.why.is_some());
    }

    struct Queue(RefCell<Vec<Envelope>>);
    impl Backend for Queue {
        async fn call(&self, request: Request) -> Result<Response> {
            match request {
                Request::DeliveryQueue { .. } => Ok(Response::Messages {
                    messages: self.0.borrow().clone(),
                }),
                Request::Inbox { drain, .. } => {
                    assert!(!drain, "channel offers must never consume the inbox");
                    Ok(Response::Messages {
                        messages: self.0.borrow().clone(),
                    })
                }
                Request::AckInbox { messages, .. } => {
                    self.0
                        .borrow_mut()
                        .retain(|message| !messages.contains(&message.id));
                    Ok(Response::Ok)
                }
                Request::ReportInput { .. } | Request::ReportAdapter { .. } => Ok(Response::Ok),
                Request::Claim { .. } => std::future::pending().await,
                other => panic!("unexpected channel request {other:?}"),
            }
        }
    }

    fn server() -> McpServer<Queue> {
        let mut server = McpServer::new(
            Queue(RefCell::new(
                (0..2)
                    .map(|index| {
                        Envelope::new(
                            if index == 0 { "user" } else { "peer" },
                            Destination::Agent("receiver".into()),
                            "chat",
                            json!({"ordinal":index}),
                            None,
                            chrono::Utc::now(),
                        )
                    })
                    .collect(),
            )),
            Identity {
                id: "receiver".into(),
                name: "fixture".into(),
                registered_here: false,
                host_pid: None,
                host_started_at: Some(chrono::Utc::now()),
            },
        );
        server.claude_channel = true;
        server
    }

    async fn receive(reader: &mut (impl AsyncBufRead + Unpin)) -> Value {
        let mut frame = Vec::new();
        let bytes = tokio::time::timeout(Duration::from_secs(2), read_frame(reader, &mut frame))
            .await
            .expect("missing channel frame")
            .unwrap()
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// A live channel with no receipt must expose the blocked FIFO instead of
    /// refreshing ready forever. Diagnostic failure never consumes or replays
    /// input, and a late receipt can recover without restarting the provider.
    #[tokio::test(start_paused = true)]
    async fn unreceived_offer_stays_paused_until_ack_even_after_failed_status_writes() {
        use agentdocker_core::{
            InputReport,
            usage::emission::{Channel, Emission},
        };
        use std::cell::Cell;
        struct Observed {
            queue: Queue,
            reports: RefCell<Vec<InputReport>>,
            emissions: RefCell<Vec<Emission>>,
            first_pause: Cell<bool>,
            failure: u8,
        }
        impl Backend for Observed {
            async fn call(&self, request: Request) -> Result<Response> {
                if let Request::ReportInput { report, .. } = &request {
                    self.reports.borrow_mut().push(report.clone());
                    if matches!(report, InputReport::Paused { .. })
                        && !self.first_pause.replace(true)
                    {
                        match self.failure {
                            1 => bail!("diagnostic write refused"),
                            2 => std::future::pending::<()>().await,
                            _ => {}
                        }
                    }
                }
                self.queue.call(request).await
            }
            async fn record_emission(&self, agent: &str, emission: Emission) {
                assert_eq!(agent, "receiver");
                self.emissions.borrow_mut().push(emission);
            }
        }
        for failure in 0..3 {
            let old = server();
            let mut server = McpServer::new(
                Observed {
                    queue: old.backend,
                    reports: RefCell::new(Vec::new()),
                    emissions: RefCell::new(Vec::new()),
                    first_pause: Cell::new(false),
                    failure,
                },
                old.identity,
            );
            server.claude_channel = true;
            let ids: Vec<_> = server
                .backend
                .queue
                .0
                .borrow()
                .iter()
                .map(|m| m.id.clone())
                .collect();
            let (transport, client) = tokio::io::duplex(8192);
            let (input, output) = tokio::io::split(transport);
            let trial = async {
                let (reader, mut writer) = tokio::io::split(client);
                let mut reader = BufReader::new(reader);
                write_line(
                    &mut writer,
                    &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                )
                .await
                .unwrap();
                let original = receive(&mut reader).await;
                assert_eq!(original["params"]["meta"]["message_id"], ids[0].as_str());
                // No receipt has been seen this session, so the first
                // offer is repeated once after the receipt timeout, under
                // the same id and saying it is a repeat; then nothing more.
                let mut frame = Vec::new();
                let repeated: Value = serde_json::from_slice(
                    &tokio::time::timeout(
                        Duration::from_secs(35),
                        read_frame(&mut reader, &mut frame),
                    )
                    .await
                    .expect("the first unreceived offer is repeated once")
                    .unwrap()
                    .unwrap(),
                )
                .unwrap();
                assert_eq!(repeated["params"]["meta"]["message_id"], ids[0].as_str());
                assert_eq!(repeated["params"]["meta"]["repeat"], "true");
                assert!(
                    tokio::time::timeout(
                        Duration::from_secs(65),
                        read_frame(&mut reader, &mut frame)
                    )
                    .await
                    .is_err()
                );
                {
                    // Reoffering the same message is a second completed
                    // output write, with a distinct accounting identity.
                    let emissions = server.backend.emissions.borrow();
                    assert_eq!(emissions.len(), 2);
                    assert_ne!(emissions[0].id, emissions[1].id);
                    for (emission, frame) in emissions.iter().zip([&original, &repeated]) {
                        assert_eq!(emission.channel, Channel::Channel);
                        assert_eq!(
                            emission.bytes,
                            serde_json::to_vec(frame).unwrap().len() as u64 + 1
                        );
                    }
                }
                {
                    let reports = server.backend.reports.borrow();
                    let first = reports
                        .iter()
                        .position(|r| matches!(r, InputReport::Paused { .. }))
                        .expect("missing durable receipt warning");
                    assert!(
                        reports.len() >= first + 2,
                        "missing retry/refresh after the first stalled report"
                    );
                    assert!(reports[first..].iter().all(|r| matches!(r, InputReport::Paused { reason } if reason.contains(ids[0].as_str()))));
                }
                assert_eq!(
                    server
                        .backend
                        .queue
                        .0
                        .borrow()
                        .iter()
                        .map(|m| m.id.clone())
                        .collect::<Vec<_>>(),
                    ids
                );
                write_line(
                    &mut writer,
                    &json!({"jsonrpc":"2.0","id":90,"method":"ping"}),
                )
                .await
                .unwrap();
                assert_eq!(receive(&mut reader).await["id"], 90);
                write_line(&mut writer, &json!({"jsonrpc":"2.0","id":91,"method":"tools/call","params":{"name":"acknowledge_messages","arguments":{"messages":[ids[0]]}}})).await.unwrap();
                assert_eq!(receive(&mut reader).await["id"], 91);
                assert_eq!(
                    receive(&mut reader).await["params"]["meta"]["message_id"],
                    ids[1].as_str()
                );
                assert!(matches!(
                    server.backend.reports.borrow().last(),
                    Some(InputReport::Ready)
                ));
                assert_eq!(server.backend.queue.0.borrow().len(), 1);
                assert!(
                    tokio::time::timeout(
                        Duration::from_secs(1),
                        read_frame(&mut reader, &mut frame)
                    )
                    .await
                    .is_err()
                );
                writer.shutdown().await.unwrap();
            };
            let (result, ()) = tokio::join!(
                pump(
                    &server,
                    BufReader::new(input),
                    stdio::AsyncOutput(output),
                    TEST_ADAPTER
                ),
                trial
            );
            result.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn provider_limits_stop_pings_but_allow_receipts_and_resume_fifo() {
        use std::cell::Cell;
        struct Limited {
            queue: Queue,
            blocked: Cell<bool>,
            availability: agentdocker_core::ProviderAvailability,
        }
        impl Backend for Limited {
            async fn call(&self, request: Request) -> Result<Response> {
                if self.blocked.get() && matches!(request, Request::DeliveryQueue { .. }) {
                    return Ok(Response::InputWaiting {
                        agent: "receiver".into(),
                        blocked_by: "receiver".into(),
                        availability: self.availability.clone(),
                        queued: self.queue.0.borrow().len(),
                    });
                }
                self.queue.call(request).await
            }
        }
        let old = server();
        let now = chrono::Utc::now();
        let mut server = McpServer::new(
            Limited {
                queue: old.backend,
                blocked: Cell::new(true),
                availability: agentdocker_core::ProviderAvailability {
                    process_started_at: now,
                    observed_at: now,
                    issue: Some(agentdocker_core::ProviderIssue::local(
                        agentdocker_core::ProviderIssueKind::Usage,
                    )),
                    cleared_observation: None,
                    retries: 0,
                },
            },
            old.identity,
        );
        server.claude_channel = true;
        let ids: Vec<_> = server
            .backend
            .queue
            .0
            .borrow()
            .iter()
            .map(|m| m.id.clone())
            .collect();
        let (transport, client) = tokio::io::duplex(8192);
        let (input, output) = tokio::io::split(transport);
        let trial = async {
            let (reader, mut writer) = tokio::io::split(client);
            let mut reader = BufReader::new(reader);
            write_line(
                &mut writer,
                &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            )
            .await
            .unwrap();
            let mut frame = Vec::new();
            assert!(
                tokio::time::timeout(Duration::from_secs(60), read_frame(&mut reader, &mut frame))
                    .await
                    .is_err()
            );
            assert_eq!(server.backend.queue.0.borrow().len(), 2);
            server.backend.blocked.set(false);
            assert_eq!(
                receive(&mut reader).await["params"]["meta"]["message_id"],
                ids[0].as_str()
            );
            server.backend.blocked.set(true);
            tokio::time::sleep(Duration::from_secs(1)).await;
            write_line(
                &mut writer,
                &json!({"jsonrpc":"2.0","id":7,"method":"tools/call",
                "params":{"name":"acknowledge_messages","arguments":{"messages":[ids[0]]}}}),
            )
            .await
            .unwrap();
            assert_eq!(receive(&mut reader).await["id"], 7);
            assert_eq!(server.backend.queue.0.borrow().len(), 1);
            assert!(
                tokio::time::timeout(Duration::from_secs(60), read_frame(&mut reader, &mut frame))
                    .await
                    .is_err()
            );
            server.backend.blocked.set(false);
            assert_eq!(
                receive(&mut reader).await["params"]["meta"]["message_id"],
                ids[1].as_str()
            );
            // A second limit/recovery must not duplicate an outstanding offer.
            server.backend.blocked.set(true);
            tokio::time::sleep(Duration::from_secs(1)).await;
            server.backend.blocked.set(false);
            assert!(
                tokio::time::timeout(Duration::from_secs(60), read_frame(&mut reader, &mut frame))
                    .await
                    .is_err()
            );
            writer.shutdown().await.unwrap();
        };
        let (result, ()) = tokio::join!(
            pump(
                &server,
                BufReader::new(input),
                stdio::AsyncOutput(output),
                TEST_ADAPTER
            ),
            trial
        );
        result.unwrap();
    }

    /// A session that asked to resume an earlier one binds input only once
    /// its hooks have named the session actually running — at once when the
    /// hook registered first, later when the channel initialized first —
    /// following the hook's word even when it differs from the command
    /// line, ignoring a record for another process generation, and past
    /// the wait binding anyway (absent hooks) and saying so. Until then the
    /// handshake, control calls and receipts are served and no message is
    /// offered.
    #[tokio::test(start_paused = true)]
    async fn readiness_waits_for_the_hooks_to_vouch_for_a_resumed_session() {
        use std::cell::{Cell, RefCell};
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        enum Vouch {
            Immediate,
            AfterPolls(usize),
            Other,
            WrongGeneration,
            MissingGeneration,
            SlowInspect,
            Never,
        }
        struct Hooks {
            queue: Queue,
            vouch: Vouch,
            inspects: Cell<usize>,
            ready_reports: Cell<usize>,
            queue_polls_before_ready: Cell<usize>,
            started: chrono::DateTime<chrono::Utc>,
            log: RefCell<Vec<&'static str>>,
        }
        impl Backend for Hooks {
            async fn call(&self, request: Request) -> Result<Response> {
                match request {
                    Request::Inspect { .. } => {
                        let n = self.inspects.get() + 1;
                        self.inspects.set(n);
                        if self.vouch == Vouch::SlowInspect {
                            // The daemon never answers: each look is bounded
                            // by the transport timeout, and the wait still ends.
                            std::future::pending::<()>().await;
                        }
                        let mut record = agentdocker_core::AgentRecord::new(
                            agentdocker_core::AgentSpec {
                                name: "fixture".into(),
                                runtime: "claude-code".into(),
                                ..Default::default()
                            },
                            false,
                            self.started,
                        );
                        record.pid = Some(4242);
                        record.process_started_at = Some(self.started);
                        let session = match self.vouch {
                            Vouch::Immediate => Some("requested-session"),
                            Vouch::AfterPolls(k) if n > k => Some("requested-session"),
                            Vouch::AfterPolls(_) => None,
                            Vouch::Other => Some("the-session-the-hook-saw"),
                            Vouch::WrongGeneration => {
                                record.process_started_at =
                                    Some(self.started - chrono::Duration::hours(1));
                                Some("requested-session")
                            }
                            Vouch::MissingGeneration => {
                                record.pid = None;
                                record.process_started_at = None;
                                Some("requested-session")
                            }
                            Vouch::SlowInspect | Vouch::Never => None,
                        };
                        if let Some(session) = session {
                            record
                                .spec
                                .labels
                                .insert("session_id".into(), session.into());
                        }
                        Ok(Response::Agent { agent: record })
                    }
                    Request::ReportInput {
                        report: agentdocker_core::InputReport::Ready,
                        ..
                    } => {
                        self.ready_reports.set(self.ready_reports.get() + 1);
                        self.log.borrow_mut().push("ready");
                        Ok(Response::Ok)
                    }
                    Request::DeliveryQueue { .. } => {
                        if self.ready_reports.get() == 0 {
                            self.queue_polls_before_ready
                                .set(self.queue_polls_before_ready.get() + 1);
                        }
                        self.log.borrow_mut().push("queue");
                        self.queue
                            .call(Request::DeliveryQueue {
                                agent: "receiver".into(),
                            })
                            .await
                    }
                    other => self.queue.call(other).await,
                }
            }
        }
        for vouch in [
            Vouch::Immediate,
            Vouch::AfterPolls(3),
            Vouch::Other,
            Vouch::WrongGeneration,
            Vouch::MissingGeneration,
            Vouch::SlowInspect,
            Vouch::Never,
        ] {
            let started = chrono::Utc::now();
            let old = server();
            let mut server = McpServer::new(
                Hooks {
                    queue: old.backend,
                    vouch,
                    inspects: Cell::new(0),
                    ready_reports: Cell::new(0),
                    queue_polls_before_ready: Cell::new(0),
                    started,
                    log: RefCell::new(Vec::new()),
                },
                Identity {
                    host_pid: Some(4242),
                    host_started_at: Some(started),
                    ..old.identity
                },
            );
            server.claude_channel = true;
            server.resume_vouch = Some(crate::mcp::ResumeVouch {
                requested: Some("requested-session".into()),
                wait: Duration::from_secs(10),
            });
            let waits = !matches!(vouch, Vouch::Immediate | Vouch::Other);
            let first_id = server.backend.queue.0.borrow()[0].id.clone();
            let (transport, client) = tokio::io::duplex(8192);
            let (input, output) = tokio::io::split(transport);
            let trial = async {
                let (reader, mut writer) = tokio::io::split(client);
                let mut reader = BufReader::new(reader);
                write_line(
                    &mut writer,
                    &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                )
                .await
                .unwrap();
                async fn receive_within(
                    reader: &mut BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
                    secs: u64,
                ) -> Value {
                    let mut frame = Vec::new();
                    let bytes = tokio::time::timeout(
                        Duration::from_secs(secs),
                        read_frame(reader, &mut frame),
                    )
                    .await
                    .expect("missing channel frame")
                    .unwrap()
                    .unwrap();
                    serde_json::from_slice::<Value>(&bytes).unwrap()
                }
                if waits {
                    // While the vouch is awaited nothing is bound or offered,
                    // yet control and an explicit receipt are answered at
                    // once: the wait costs the session nothing it needs.
                    write_line(
                        &mut writer,
                        &json!({"jsonrpc":"2.0","id":1,"method":"ping"}),
                    )
                    .await
                    .unwrap();
                    let reply = receive_within(&mut reader, 1).await;
                    assert_eq!(reply["id"], 1, "{vouch:?}: {reply}");
                    write_line(
                        &mut writer,
                        &json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                        "params":{"name":"acknowledge_messages","arguments":{"messages":[first_id]}}}),
                    )
                    .await
                    .unwrap();
                    let reply = receive_within(&mut reader, 1).await;
                    assert_eq!(reply["id"], 2, "{vouch:?}: {reply}");
                    assert_eq!(
                        server.backend.ready_reports.get(),
                        0,
                        "{vouch:?}: not yet bound"
                    );
                    assert_eq!(
                        server.backend.queue.0.borrow().len(),
                        1,
                        "{vouch:?}: the receipt landed"
                    );
                }
                // Then, once input is bound — after the full wait, for a
                // vouch that never comes — the first message is offered.
                let offer = receive_within(&mut reader, 30).await;
                assert!(
                    offer["params"]["meta"]["message_id"].is_string(),
                    "{vouch:?}: {offer}"
                );
                assert_eq!(server.backend.ready_reports.get(), 1, "{vouch:?}");
                writer.shutdown().await.unwrap();
            };
            let (result, ()) = tokio::join!(
                pump(
                    &server,
                    BufReader::new(input),
                    stdio::AsyncOutput(output),
                    TEST_ADAPTER
                ),
                trial
            );
            result.unwrap();
            let backend = &server.backend;
            assert_eq!(backend.ready_reports.get(), 1, "{vouch:?}: one binding");
            assert_eq!(
                backend.queue_polls_before_ready.get(),
                0,
                "{vouch:?}: nothing offered before input is bound"
            );
            assert_eq!(backend.log.borrow().first(), Some(&"ready"), "{vouch:?}");
            let inspects = backend.inspects.get();
            match vouch {
                Vouch::Immediate | Vouch::Other => {
                    assert_eq!(inspects, 1, "{vouch:?}: vouched on the first look")
                }
                Vouch::AfterPolls(k) => assert_eq!(inspects, k + 1, "{vouch:?}"),
                // Never vouched: looked until the ten-second wait, then bound.
                Vouch::WrongGeneration | Vouch::MissingGeneration | Vouch::Never => {
                    assert!(
                        inspects >= 30,
                        "{vouch:?}: waited the full deadline ({inspects} looks)"
                    )
                }
                // Each look was bounded by the transport timeout, so the
                // wait ended on the deadline rather than hanging on one.
                Vouch::SlowInspect => {
                    assert!(
                        (4..=6).contains(&inspects),
                        "{vouch:?}: {inspects} bounded looks"
                    )
                }
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_refusal_and_timeout_preserve_the_channel_and_exact_receipts() {
        use std::cell::Cell;
        struct Heartbeat {
            queue: Queue,
            reports: Cell<usize>,
        }
        impl Backend for Heartbeat {
            async fn call(&self, request: Request) -> Result<Response> {
                if matches!(
                    request,
                    Request::ReportInput {
                        report: agentdocker_core::InputReport::Ready
                            | agentdocker_core::InputReport::Paused { .. },
                        ..
                    }
                ) {
                    let count = self.reports.get() + 1;
                    self.reports.set(count);
                    return match count {
                        2 => Ok(Response::error(
                            agentdocker_core::ErrorCode::StorageUnavailable,
                            "fixture refusal",
                        )),
                        3 => std::future::pending().await,
                        _ => Ok(Response::Ok),
                    };
                }
                self.queue.call(request).await
            }
        }
        let fixture = server();
        let mut server = McpServer::new(
            Heartbeat {
                queue: fixture.backend,
                reports: Cell::new(0),
            },
            fixture.identity,
        );
        server.claude_channel = true;
        let head = server.backend.queue.0.borrow()[0].id.clone();
        let (transport, client) = tokio::io::duplex(8192);
        let (input, output) = tokio::io::split(transport);
        let trial = async {
            let (reader, mut writer) = tokio::io::split(client);
            let mut reader = BufReader::new(reader);
            write_line(
                &mut writer,
                &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            )
            .await
            .unwrap();
            assert_eq!(
                receive(&mut reader).await["params"]["meta"]["message_id"],
                head.as_str()
            );
            assert_eq!(server.backend.reports.get(), 1);
            // Refusal, a stalled write and recovery must all preserve the
            // outstanding offer and leave ordinary requests serviceable.
            for expected in 2..=4 {
                tokio::time::advance(Duration::from_secs(31)).await;
                tokio::time::sleep(Duration::from_millis(300)).await;
                if expected == 2 {
                    // No receipt has been seen yet, so the head is offered
                    // a second time at the receipt timeout, before anything
                    // else is written.
                    let repeat = receive(&mut reader).await;
                    assert_eq!(repeat["params"]["meta"]["message_id"], head.as_str());
                    assert_eq!(repeat["params"]["meta"]["repeat"], "true");
                }
                write_line(
                    &mut writer,
                    &json!({"jsonrpc":"2.0","id":expected,"method":"ping"}),
                )
                .await
                .unwrap();
                assert_eq!(receive(&mut reader).await["id"], expected);
                assert_eq!(server.backend.reports.get(), expected);
                assert_eq!(server.backend.queue.0.borrow().len(), 2);
            }
            write_line(
                &mut writer,
                &json!({"jsonrpc":"2.0","id":9,"method":"tools/call",
                "params":{"name":"acknowledge_messages","arguments":{"messages":[head]}}}),
            )
            .await
            .unwrap();
            assert_eq!(receive(&mut reader).await["id"], 9);
            assert_eq!(server.backend.queue.0.borrow().len(), 1);
            assert_eq!(
                receive(&mut reader).await["params"]["meta"]["message_id"],
                server.backend.queue.0.borrow()[0].id.as_str()
            );
            writer.shutdown().await.unwrap();
        };
        let (result, ()) = tokio::join!(
            pump(
                &server,
                BufReader::new(input),
                stdio::AsyncOutput(output),
                TEST_ADAPTER
            ),
            trial
        );
        result.unwrap();
        assert_eq!(server.backend.reports.get(), 5);
    }

    #[tokio::test(start_paused = true)]
    async fn channel_waits_for_initialization_retains_offers_and_receipts_bypass_waiting_calls() {
        let server = server();
        let ids: Vec<_> = server
            .backend
            .0
            .borrow()
            .iter()
            .map(|message| message.id.clone())
            .collect();
        let (transport, client) = tokio::io::duplex(8192);
        let (input, output) = tokio::io::split(transport);
        let trial = async {
            let (reader, mut writer) = tokio::io::split(client);
            let mut reader = BufReader::new(reader);
            let mut frame = Vec::new();
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(300),
                    read_frame(&mut reader, &mut frame)
                )
                .await
                .is_err()
            );
            write_line(
                &mut writer,
                &json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
            )
            .await
            .unwrap();
            assert_eq!(
                receive(&mut reader).await["result"]["capabilities"]["experimental"]["claude/channel"],
                json!({})
            );
            write_line(
                &mut writer,
                &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            )
            .await
            .unwrap();
            // A provider whose channel handler is installed shortly after
            // initialization must not lose the first retained queue item.
            // Control replies still work during the settling interval.
            write_line(
                &mut writer,
                &json!({"jsonrpc":"2.0","id":99,"method":"ping"}),
            )
            .await
            .unwrap();
            assert_eq!(receive(&mut reader).await["id"], 99);
            assert!(
                tokio::time::timeout(FIRST_OFFER_SETTLE / 2, read_frame(&mut reader, &mut frame))
                    .await
                    .is_err()
            );
            assert_eq!(server.backend.0.borrow().len(), 2);
            let offered = receive(&mut reader).await;
            assert_eq!(offered["params"]["meta"]["message_id"], ids[0].as_str());
            assert_eq!(offered["params"]["meta"]["from_agent"], "user");
            assert_eq!(server.backend.0.borrow().len(), 2);
            // Fill every long-running slot. The receipt lane must remain usable.
            for id in 10..10 + ACTIVE_CALLS {
                write_line(
                    &mut writer,
                    &json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
                    "params":{"name":"claim","arguments":{"resource":format!("task:waiting-{id}"),"wait_secs":120}}}),
                )
                .await
                .unwrap();
            }
            write_line(
                &mut writer,
                &json!({"jsonrpc":"2.0","id":2,"method":"ping"}),
            )
            .await
            .unwrap();
            assert_eq!(receive(&mut reader).await["id"], 2);
            write_line(
                &mut writer,
                &json!([{ "jsonrpc":"2.0","id":3,"method":"tools/call",
                "params":{"name":"acknowledge_messages","arguments":{"messages":[ids[0]]}}},
                {"jsonrpc":"2.0","id":4,"method":"ping"}]),
            )
            .await
            .unwrap();
            let receipt = receive(&mut reader).await;
            assert_eq!(receipt[0]["id"], 3);
            assert_eq!(receipt[1]["id"], 4);
            assert!(receipt[0].get("error").is_none(), "{receipt}");
            let next = receive(&mut reader).await;
            assert_eq!(next["params"]["meta"]["message_id"], ids[1].as_str());
            assert_eq!(next["params"]["meta"]["from_agent"], "peer");
            assert_eq!(server.backend.0.borrow().len(), 1);
            // No periodic duplicate offer before an explicit receipt.
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(300),
                    read_frame(&mut reader, &mut frame)
                )
                .await
                .is_err()
            );
            writer.shutdown().await.unwrap();
        };
        let (result, ()) = tokio::join!(
            pump(
                &server,
                BufReader::new(input),
                stdio::AsyncOutput(output),
                TEST_ADAPTER
            ),
            trial
        );
        result.unwrap();
        assert_eq!(server.backend.0.borrow()[0].id, ids[1]);
    }

    /// Once a receipt has been seen, the handler is known to be there: a
    /// later offer that goes unreceived is never repeated, only reported.
    #[tokio::test(start_paused = true)]
    async fn a_later_unreceived_offer_is_not_repeated_once_a_receipt_has_been_seen() {
        let server = server();
        let ids: Vec<_> = server
            .backend
            .0
            .borrow()
            .iter()
            .map(|m| m.id.clone())
            .collect();
        let (transport, client) = tokio::io::duplex(8192);
        let (input, output) = tokio::io::split(transport);
        let trial = async {
            let (reader, mut writer) = tokio::io::split(client);
            let mut reader = BufReader::new(reader);
            write_line(
                &mut writer,
                &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            )
            .await
            .unwrap();
            let first = receive(&mut reader).await;
            assert_eq!(first["params"]["meta"]["message_id"], ids[0].as_str());
            assert!(first["params"]["meta"].get("repeat").is_none());
            write_line(&mut writer, &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"acknowledge_messages","arguments":{"messages":[ids[0]]}}})).await.unwrap();
            assert_eq!(receive(&mut reader).await["id"], 1);
            let second = receive(&mut reader).await;
            assert_eq!(second["params"]["meta"]["message_id"], ids[1].as_str());
            let mut frame = Vec::new();
            assert!(
                tokio::time::timeout(Duration::from_secs(95), read_frame(&mut reader, &mut frame))
                    .await
                    .is_err(),
                "an unreceived offer after a seen receipt is not repeated"
            );
            assert_eq!(server.backend.0.borrow().len(), 1);
            writer.shutdown().await.unwrap();
        };
        let (result, ()) = tokio::join!(
            pump(
                &server,
                BufReader::new(input),
                stdio::AsyncOutput(output),
                TEST_ADAPTER
            ),
            trial
        );
        result.unwrap();
    }

    /// A receipt is proof of the handler whichever message it names: one
    /// the daemon accepts before the first offer (a session resumed with
    /// a message it had already read, say) means no later offer is
    /// repeated. The diagnostic path alone does not see that receipt,
    /// because the message it names was never offered on this channel.
    #[tokio::test(start_paused = true)]
    async fn a_receipt_accepted_before_the_first_offer_counts_as_seen() {
        let server = server();
        let ids: Vec<_> = server
            .backend
            .0
            .borrow()
            .iter()
            .map(|m| m.id.clone())
            .collect();
        let (transport, client) = tokio::io::duplex(8192);
        let (input, output) = tokio::io::split(transport);
        let trial = async {
            let (reader, mut writer) = tokio::io::split(client);
            let mut reader = BufReader::new(reader);
            // A refused receipt proves nothing; it is served either way.
            write_line(&mut writer, &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"acknowledge_messages","arguments":{"messages":[]}}})).await.unwrap();
            let refused = receive(&mut reader).await;
            assert_eq!(refused["id"], 1);
            assert!(
                refused.get("error").is_some() || refused["result"]["isError"] == true,
                "{refused}"
            );
            assert_eq!(server.backend.0.borrow().len(), 2);
            write_line(&mut writer, &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"acknowledge_messages","arguments":{"messages":[ids[0]]}}})).await.unwrap();
            let accepted = receive(&mut reader).await;
            assert_eq!(accepted["id"], 2);
            assert!(accepted.get("error").is_none(), "{accepted}");
            assert_eq!(server.backend.0.borrow().len(), 1);
            write_line(
                &mut writer,
                &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            )
            .await
            .unwrap();
            let first = receive(&mut reader).await;
            assert_eq!(first["params"]["meta"]["message_id"], ids[1].as_str());
            assert!(first["params"]["meta"].get("repeat").is_none());
            let mut frame = Vec::new();
            assert!(
                tokio::time::timeout(Duration::from_secs(95), read_frame(&mut reader, &mut frame))
                    .await
                    .is_err(),
                "no offer is repeated once a receipt has been accepted"
            );
            assert_eq!(server.backend.0.borrow().len(), 1);
            writer.shutdown().await.unwrap();
        };
        let (result, ()) = tokio::join!(
            pump(
                &server,
                BufReader::new(input),
                stdio::AsyncOutput(output),
                TEST_ADAPTER
            ),
            trial
        );
        result.unwrap();
    }

    #[tokio::test]
    async fn reconnect_reoffers_the_same_unacknowledged_head_without_consuming_it() {
        let server = server();
        let id = server.backend.0.borrow()[0].id.clone();
        for _ in 0..2 {
            let (transport, client) = tokio::io::duplex(8192);
            let (input, output) = tokio::io::split(transport);
            let trial = async {
                let (reader, mut writer) = tokio::io::split(client);
                let mut reader = BufReader::new(reader);
                write_line(
                    &mut writer,
                    &json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
                )
                .await
                .unwrap();
                assert_eq!(receive(&mut reader).await["id"], 1);
                write_line(
                    &mut writer,
                    &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                )
                .await
                .unwrap();
                assert_eq!(
                    receive(&mut reader).await["params"]["meta"]["message_id"],
                    id.as_str()
                );
                writer.shutdown().await.unwrap();
            };
            let (result, ()) = tokio::join!(
                pump(
                    &server,
                    BufReader::new(input),
                    stdio::AsyncOutput(output),
                    TEST_ADAPTER
                ),
                trial
            );
            result.unwrap();
        }
        assert_eq!(server.backend.0.borrow().len(), 2);
    }

    #[tokio::test]
    async fn frame_cancellation_preserves_partial_input_and_oversized_frames_are_refused() {
        let (mut writer, reader) = tokio::io::duplex(1024);
        let mut reader = BufReader::new(reader);
        writer.write_all(b"{\"id\":").await.unwrap();
        let mut frame = Vec::new();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(10),
                read_frame(&mut reader, &mut frame)
            )
            .await
            .is_err()
        );
        writer.write_all(b"1}\n").await.unwrap();
        assert_eq!(
            read_frame(&mut reader, &mut frame).await.unwrap().unwrap(),
            b"{\"id\":1}\n"
        );
        let oversized = vec![b'x'; FRAME_BYTES + 1];
        assert!(
            read_frame(&mut oversized.as_slice(), &mut frame)
                .await
                .is_err()
        );
        assert!(frame.len() <= FRAME_BYTES);
    }
}
