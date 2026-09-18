//! Opt-in Claude channel input over the existing private daemon queue.
//! A completed stdout write is only an offer. The durable envelope remains
//! until a model ACK or verified provider transcript confirms actual input.
use super::{Backend, Identity, McpServer, error_response};
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

pub(super) async fn serve(server: &McpServer<impl Backend>) -> Result<()> {
    let (input, output) = stdio::open()?;
    pump(server, input, output).await
}

const FRAME_BYTES: usize = 1024 * 1024;
const ACTIVE_CALLS: usize = 8;
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_EVERY: Duration = Duration::from_millis(250);
const RECEIPT_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) struct Owner {
    _process: lock::Lock,
    _agent: lock::Lock,
}

pub(super) fn acquire(identity: &Identity) -> Result<Owner> {
    acquire_for(&dirs::home(), identity)
}

fn process_key(pid: u32, started: chrono::DateTime<chrono::Utc>) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "process-{pid}-{:x}",
        Sha256::digest(started.to_rfc3339().as_bytes())
    )
}

fn acquire_for(home: &std::path::Path, identity: &Identity) -> Result<Owner> {
    let pid = identity
        .host_pid
        .context("channel provider PID is unavailable")?;
    let started = identity
        .host_started_at
        .context("channel provider generation is unavailable")?;
    // The process lock remains the same if SessionStart canonicalizes this
    // registration before channel initialization. An agent-ID lock alone would
    // let a second MCP entry acquire the canonical ID while we hold its alias.
    let process = acquire_at(home, &process_key(pid, started))?;
    let agent = acquire_at(home, &identity.id)?;
    Ok(Owner {
        _process: process,
        _agent: agent,
    })
}

fn directory(home: &std::path::Path, id: &str) -> Result<std::path::PathBuf> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail!("invalid agent ID for channel ownership");
    }
    Ok(home.join("channel-input"))
}

fn acquire_at(home: &std::path::Path, id: &str) -> Result<lock::Lock> {
    let directory = directory(home, id)?;
    dirs::ensure_private_dir(&directory)?;
    let path = directory.join(format!("{id}.lock"));
    dirs::private_file(&path, true, false)?;
    lock::try_exclusive_existing(&path)?
        .context("another channel adapter already serves this agent; close the duplicate MCP entry")
}

pub(super) fn active(home: &std::path::Path, id: &str) -> Result<bool> {
    let directory = directory(home, id)?;
    match std::fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        result => {
            result?;
        }
    }
    dirs::ensure_private_dir(&directory)?;
    match lock::try_exclusive_existing(&directory.join(format!("{id}.lock"))) {
        Ok(held) => Ok(held.is_none()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn active_for(
    home: &std::path::Path,
    agent: &agentdocker_core::AgentRecord,
) -> Result<bool> {
    if active(home, agent.id.as_str())? {
        return Ok(true);
    }
    match (agent.pid, agent.process_started_at) {
        (Some(pid), Some(started)) => active(home, &process_key(pid, started)),
        _ => Ok(false),
    }
}

type Pending<'a> = Pin<Box<dyn Future<Output = Option<Value>> + 'a>>;

/// The channel transport keeps receipt calls responsive even while another
/// tool waits for a human answer. All stdout frames use this single writer.
async fn pump<B: Backend, R: AsyncBufRead + Unpin, W: stdio::Output>(
    server: &McpServer<B>,
    mut input: R,
    mut output: W,
) -> Result<()> {
    let mut pending: Vec<Pending<'_>> = Vec::new();
    let mut frame = Vec::new();
    let mut initialized = false;
    // Input delivery is bound — readiness reported — once the handshake
    // is done and, for a session that asked to resume an earlier one,
    // once its hooks have said which session runs (or the wait expired).
    let mut ready = false;
    let mut vouch_deadline: Option<tokio::time::Instant> = None;
    let mut vouch_tick = tokio::time::interval(POLL_EVERY);
    vouch_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // One look at the daemon's record at a time, polled beside the
    // transport so a slow daemon never holds up control or receipts.
    let mut look: Option<Pin<Box<dyn Future<Output = Option<String>> + '_>>> = None;
    let mut tick = tokio::time::interval(POLL_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut offered: Option<(MessageId, tokio::time::Instant, bool)> = None;
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
                        write(&mut output, &error_response(Value::Null, super::PARSE_ERROR, &format!("parse error: {error}"))).await?;
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
                    match tokio::time::timeout(IO_TIMEOUT, server.handle_incoming(value)).await {
                        Ok(Some(response)) => write(&mut output, &response).await?,
                        Ok(None) => {},
                        Err(_) => write(&mut output, &error_response(id, super::INTERNAL_ERROR, "receipt/control request timed out; inspect the retained inbox before retrying")).await?,
                    }
                } else if pending.len() < ACTIVE_CALLS {
                    pending.push(Box::pin(server.handle_incoming(value)));
                } else if let Some(id) = value.get("id") {
                    write(&mut output, &error_response(id.clone(), -32000, "MCP request capacity is full; retry this request later")).await?;
                } else if value.is_array() {
                    // Reject every request in an over-capacity legacy batch.
                    let replies: Vec<_> = value.as_array().unwrap().iter().filter_map(|item| item.get("id")).map(|id|
                        error_response(id.clone(), -32000, "MCP request capacity is full; retry this request later")
                    ).collect();
                    if !replies.is_empty() { write(&mut output, &Value::Array(replies)).await?; }
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
                if let Some(response) = response { write(&mut output, &response).await?; }
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
                if let Some((id, since, warned)) = &mut offered {
                    if messages.iter().any(|message| &message.id == id) {
                        if !*warned && since.elapsed() >= RECEIPT_TIMEOUT {
                            eprintln!("agentdocker channel: message {id} has no receipt after 30 seconds; verify this session's channel opt-in and permissions");
                            *warned = true;
                            state_changed = true;
                        }
                    } else {
                        state_changed = *warned;
                        offered = None;
                    }
                }
                // Queue reachability is not evidence that the provider consumed
                // an offer. Keep a missing-receipt state visible until that ID
                // leaves the queue, including after failed diagnostic writes.
                if state_changed || last_report.elapsed() >= Duration::from_secs(30) {
                    let observation = match &offered {
                        Some((id, _, true)) => agentdocker_core::InputReport::Paused {
                            reason: format!("Waiting for the session to acknowledge message {id}; following messages remain queued. Check the session's channel permission or provider limit before resending."),
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
                if offered.is_some() {
                    continue;
                }
                if let Some(message) = messages.first() {
                    let mut notification = json!({
                        "jsonrpc": "2.0", "method": "notifications/claude/channel",
                        "params": {"content": serde_json::to_string(&message.payload)?, "meta": {
                            "message_id": message.id.as_str(), "from_agent": message.from,
                            "kind": message.kind, "sent_at": message.sent_at.to_rfc3339(),
                            "destination": serde_json::to_string(&message.to)?,
                            "reply_destination": reply_destination(message),
                            "delivery_rule": "Acknowledge this message_id after receiving the full body. Reply using send_message to reply_destination with reply_to=message_id so the response appears in the original app conversation. A human pause request requires stopping work and reporting that actual state there; a terminal-only answer is not an app reply.",
                        }}
                    });
                    if let Some(question) = &message.reply_to {
                        notification["params"]["meta"]["reply_to"] = json!(question.as_str());
                    }
                    write(&mut output, &notification).await?;
                    offered = Some((message.id.clone(), tokio::time::Instant::now(), false));
                }
            }
        }
    }
}

fn reply_destination(message: &agentdocker_core::Envelope) -> String {
    use agentdocker_core::Destination;
    match &message.to {
        Destination::Agent(_) => message.from.clone(),
        Destination::Project(project) => format!("project:{project}"),
        Destination::Channel(channel) => format!("channel:{channel}"),
        Destination::Topic(topic) => format!("topic:{topic}"),
        Destination::Broadcast => "all".into(),
    }
}

fn priority(value: &Value) -> bool {
    match value {
        Value::Array(requests) => !requests.is_empty() && requests.iter().all(priority_request),
        request => priority_request(request),
    }
}

fn priority_request(value: &Value) -> bool {
    matches!(value["method"].as_str(), Some("initialize" | "ping"))
        || (value["method"] == "tools/call" && value["params"]["name"] == "acknowledge_messages")
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
}

async fn write(output: &mut impl stdio::Output, value: &Value) -> Result<()> {
    tokio::time::timeout(WRITE_TIMEOUT, output.send(value))
        .await
        .context("channel output stalled; queued messages remain unacknowledged")?
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
    use agentdocker_core::{Destination, Envelope};
    use std::cell::RefCell;
    use tokio::io::{AsyncWriteExt, BufReader};

    #[test]
    fn channel_ownership_is_exclusive_and_detectable_without_creating_probe_state() {
        let home = tempfile::tempdir().unwrap();
        assert!(!active(home.path(), "receiver").unwrap());
        assert!(!home.path().join("channel-input").exists());
        let owner = acquire_at(home.path(), "receiver").unwrap();
        assert!(active(home.path(), "receiver").unwrap());
        assert!(acquire_at(home.path(), "receiver").is_err());
        assert!(!active(home.path(), "other").unwrap());
        drop(owner);
        // Concurrent tests can fork between open and drop; the inherited
        // descriptor holds flock until that child execs (CLOEXEC).
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while active(home.path(), "receiver").unwrap() {
            assert!(
                std::time::Instant::now() < deadline,
                "channel ownership never released"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(active(home.path(), "../escape").is_err());
    }

    #[test]
    fn channel_ownership_survives_canonical_agent_id_changes() {
        let home = tempfile::tempdir().unwrap();
        let birth = chrono::Utc::now();
        let first = Identity {
            id: "fresh".into(),
            name: "fixture".into(),
            host_pid: Some(1234),
            host_started_at: Some(birth),
            registered_here: false,
        };
        let owner = acquire_for(home.path(), &first).unwrap();
        let canonical = Identity {
            id: "canonical".into(),
            ..first.clone()
        };
        assert!(
            acquire_for(home.path(), &canonical).is_err(),
            "a new agent ID cannot admit another channel for the same process"
        );
        let mut record = agentdocker_core::AgentRecord::new(Default::default(), false, birth);
        record.id = "canonical".into();
        record.pid = first.host_pid;
        record.process_started_at = Some(birth);
        assert!(
            active_for(home.path(), &record).unwrap(),
            "hooks find the process lock even after ID folding"
        );
        record.process_started_at = Some(birth + chrono::Duration::seconds(1));
        assert!(
            !active_for(home.path(), &record).unwrap(),
            "a reused PID is another generation"
        );
        let next = Identity {
            host_started_at: record.process_started_at,
            ..canonical.clone()
        };
        let next_owner = acquire_for(home.path(), &next).unwrap();
        drop(next_owner);
        drop(owner);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match acquire_for(home.path(), &canonical) {
                Ok(_) => break,
                Err(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("channel ownership did not release: {error}"),
            }
        }
        let unknown = Identity {
            host_started_at: None,
            ..canonical
        };
        assert!(acquire_for(home.path(), &unknown).is_err());
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
        use agentdocker_core::InputReport;
        use std::cell::Cell;
        struct Observed {
            queue: Queue,
            reports: RefCell<Vec<InputReport>>,
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
        }
        for failure in 0..3 {
            let old = server();
            let mut server = McpServer::new(
                Observed {
                    queue: old.backend,
                    reports: RefCell::new(Vec::new()),
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
                assert_eq!(
                    receive(&mut reader).await["params"]["meta"]["message_id"],
                    ids[0].as_str()
                );
                let mut frame = Vec::new();
                assert!(
                    tokio::time::timeout(
                        Duration::from_secs(65),
                        read_frame(&mut reader, &mut frame)
                    )
                    .await
                    .is_err()
                );
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
                pump(&server, BufReader::new(input), stdio::AsyncOutput(output)),
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
            pump(&server, BufReader::new(input), stdio::AsyncOutput(output)),
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
                pump(&server, BufReader::new(input), stdio::AsyncOutput(output)),
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
            pump(&server, BufReader::new(input), stdio::AsyncOutput(output)),
            trial
        );
        result.unwrap();
        assert_eq!(server.backend.reports.get(), 5);
    }

    #[tokio::test]
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
            pump(&server, BufReader::new(input), stdio::AsyncOutput(output)),
            trial
        );
        result.unwrap();
        assert_eq!(server.backend.0.borrow()[0].id, ids[1]);
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
                pump(&server, BufReader::new(input), stdio::AsyncOutput(output)),
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
