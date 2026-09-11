//! Opt-in Claude channel input over the existing private daemon queue.
//! A completed stdout write is only an offer. The durable envelope remains
//! until the model explicitly acknowledges its stable ID through an MCP tool.
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

pub(super) fn acquire(identity: &Identity) -> Result<lock::Lock> {
    acquire_at(&dirs::home(), &identity.id)
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
    let mut tick = tokio::time::interval(POLL_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut offered: Option<(MessageId, tokio::time::Instant, bool)> = None;
    let mut unavailable = false;
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
                    initialized = true;
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
            _ = tick.tick(), if initialized => {
                let reply = tokio::time::timeout(IO_TIMEOUT, server.backend.call(Request::Inbox {
                    agent: server.identity.id.clone(), drain: false,
                })).await;
                let messages = match reply {
                    Ok(Ok(Response::Messages { messages })) => { unavailable = false; messages },
                    _ => {
                        if !unavailable {
                            eprintln!("agentdocker channel: inbox unavailable; no delivery is confirmed and accepted messages remain queued");
                            unavailable = true;
                        }
                        continue;
                    }
                };
                if let Some((id, since, warned)) = &mut offered {
                    if messages.iter().any(|message| &message.id == id) {
                        if !*warned && since.elapsed() >= Duration::from_secs(30) {
                            eprintln!("agentdocker channel: message {id} has no receipt after 30 seconds; verify this session's channel opt-in and permissions");
                            *warned = true;
                        }
                        continue;
                    }
                    offered = None;
                }
                if let Some(message) = messages.first() {
                    let notification = json!({
                        "jsonrpc": "2.0", "method": "notifications/claude/channel",
                        "params": {"content": serde_json::to_string(&message.payload)?, "meta": {
                            "message_id": message.id.as_str(), "from_agent": message.from,
                            "kind": message.kind, "sent_at": message.sent_at.to_rfc3339(),
                            "destination": serde_json::to_string(&message.to)?,
                        }}
                    });
                    write(&mut output, &notification).await?;
                    offered = Some((message.id.clone(), tokio::time::Instant::now(), false));
                }
            }
        }
    }
}

fn priority(value: &Value) -> bool {
    matches!(value["method"].as_str(), Some("initialize" | "ping"))
        || (value["method"] == "tools/call" && value["params"]["name"] == "acknowledge_messages")
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
        assert!(!active(home.path(), "receiver").unwrap());
        assert!(active(home.path(), "../escape").is_err());
    }

    struct Queue(RefCell<Vec<Envelope>>);
    impl Backend for Queue {
        async fn call(&self, request: Request) -> Result<Response> {
            match request {
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
                Request::Ask { .. } => std::future::pending().await,
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
                host_started_at: None,
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
                    "params":{"name":"ask_human","arguments":{"question":"waiting"}}}),
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
                &json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
                "params":{"name":"acknowledge_messages","arguments":{"messages":[ids[0]]}}}),
            )
            .await
            .unwrap();
            assert_eq!(receive(&mut reader).await["id"], 3);
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
