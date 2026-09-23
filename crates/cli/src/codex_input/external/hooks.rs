//! Active-turn delivery through the same controller that owns idle input.
//! A live provider hook holds the TUI at a tool boundary. The controller reserves
//! its offer, removes only its own queued submission, then returns context.
//! Output is not a receipt: only the exact persisted hookPrompt permits ACK.
use super::super::mcp_answers::Origin;
pub(super) use super::local::Listener;
use super::local::monotonic_millis;
use super::{Binding, Client, Ledger, Provider, answers, identity, ledger, local, queue, receipts};
use agentdocker_core::{AgentRecord, Envelope, MessageId, ProcessIdentity};
use agentdocker_host::{dirs, ipc, procinfo};
use anyhow::{Context, Result, ensure};
use ipc::Stream;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{path::Path, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    time::timeout,
};

const LIMIT: usize = 6000;
const BUDGET: Duration = Duration::from_secs(3);
const PROTOCOL: u32 = 2;
const OUTPUT_RESERVE: Duration = Duration::from_millis(200);

#[derive(Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Scope {
    Root,
    Subagent,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    // Mandatory: old clients discarded Codex's child identity and cannot
    // safely offer additionalContext, even with the right parent PID/session.
    protocol: u32,
    scope: Scope,
    expires_ms: u64,
    process: ProcessIdentity,
    session: String,
    event: String,
    nonce: String,
}

impl Request {
    fn remaining(&self) -> Result<Duration> {
        let millis = self.expires_ms.saturating_sub(monotonic_millis()?);
        ensure!(
            millis > 0 && millis <= BUDGET.as_millis() as u64,
            "native hook deadline expired or invalid"
        );
        Ok(Duration::from_millis(millis))
    }
}

async fn frame(stream: &mut Stream, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    BufReader::new(stream.take((limit + 1) as u64))
        .read_until(b'\n', &mut bytes)
        .await?;
    ensure!(
        bytes.last() == Some(&b'\n') && bytes.len() <= limit,
        "invalid native hook frame"
    );
    Ok(bytes)
}

pub async fn context(
    agent: &AgentRecord,
    event: &str,
    session: &str,
    outer_deadline: tokio::time::Instant,
) -> Result<Option<String>> {
    if !matches!(event, "PreToolUse" | "PostToolUse") {
        return Ok(None);
    }
    let available = outer_deadline.saturating_duration_since(tokio::time::Instant::now());
    let Some(budget) = available
        .checked_sub(OUTPUT_RESERVE)
        .filter(|b| !b.is_zero())
    else {
        return Ok(None);
    };
    let budget = budget.min(BUDGET);
    timeout(budget, async {
        let expires_ms = monotonic_millis()?
            .checked_add(budget.as_millis() as u64)
            .context("native hook deadline overflow")?;
        let path = local::endpoint(&dirs::home(), agent.id.as_str(), "hook")?;
        let mut stream = match Stream::connect(path).await {
            Ok(stream) => stream,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        };
        let process = ProcessIdentity {
            pid: std::process::id(),
            started_at: procinfo::start_time(std::process::id())
                .context("hook process birth unavailable")?,
        };
        let request = Request {
            protocol: PROTOCOL,
            scope: Scope::Root,
            expires_ms,
            process,
            event: event.into(),
            session: session.into(),
            nonce: uuid::Uuid::new_v4().simple().to_string(),
        };
        let mut data = serde_json::to_vec(&request)?;
        data.push(b'\n');
        stream.write_all(&data).await?;
        let result: serde_json::Value =
            serde_json::from_slice(&frame(&mut stream, 32 * 1024).await?)?;
        ensure!(
            result["nonce"].as_str() == Some(&request.nonce),
            "native hook response changed identity"
        );
        let context = result["context"].as_str().map(str::to_owned);
        ensure!(
            context.as_ref().is_none_or(|s| s.len() <= LIMIT),
            "native hook response exceeded limit"
        );
        Ok(context)
    })
    .await
    .context("native hook response timed out; input retained for reconciliation")?
}

fn verified(request: &Request, binding: &Binding) -> Result<bool> {
    if request.protocol != PROTOCOL
        || request.scope != Scope::Root
        || request.remaining().is_err()
        || request.session != binding.provider.session
        || !matches!(request.event.as_str(), "PreToolUse" | "PostToolUse")
        || request.nonce.len() != 32
        || !request.nonce.bytes().all(|b| b.is_ascii_hexdigit())
        || procinfo::start_time(request.process.pid) != Some(request.process.started_at)
    {
        return Ok(false);
    }
    let table = procinfo::processes()?;
    let mut pid = request.process.pid;
    for _ in 0..12 {
        let Some(p) = table.iter().find(|p| p.pid == pid) else {
            return Ok(false);
        };
        if p.ppid == binding.provider.process.pid {
            return Ok(procinfo::start_time(p.ppid) == Some(binding.provider.process.started_at));
        }
        if p.ppid == pid {
            break;
        }
        pid = p.ppid;
    }
    Ok(false)
}

pub(super) fn compact_context(input: &str) -> Result<Option<String>> {
    if input.len() <= LIMIT {
        return Ok(Some(input.into()));
    }
    let mut value: serde_json::Value = serde_json::from_str(input)?;
    let envelope = &mut value["agentdocker_message"];
    // Generated stale notices repeat every path in several fields. Keep their
    // original identity and complete path list, replacing only that repetition.
    if envelope["from"] == "agentd" && envelope["kind"] == "stale" {
        let paths = envelope["payload"]["paths"].clone();
        envelope["payload"] = json!({"paths":paths,"text":"Files changed after observation. Reread current content before editing. Repeated change metadata omitted from this delivery summary; the full record remains in AgentDocker history."});
        let text = serde_json::to_string(&value)?;
        return Ok((text.len() <= LIMIT).then_some(text));
    }
    Ok(None)
}

async fn authenticated_request(stream: &mut Stream) -> Result<Request> {
    // Socket permissions admit this user, but its other processes must not be
    // able to claim the PID of a real hook. Bind the request to kernel identity.
    let peer = ipc::peer_pid(stream).context("native hook peer credentials unavailable")?;
    let request: Request = serde_json::from_slice(&frame(stream, 2048).await?)?;
    ensure!(
        request.protocol == PROTOCOL && request.scope == Scope::Root,
        "native hook requires a root-scoped client"
    );
    request.remaining()?;
    ensure!(
        peer == request.process.pid,
        "native hook process does not match its socket peer"
    );
    Ok(request)
}

pub(super) async fn serve(
    mut stream: Stream,
    client: &Client,
    provider: &mut Provider,
    ledger: &mut Ledger,
    origin: &Origin,
) -> Result<()> {
    // Two bounds, each on its own step: authentication gets two seconds
    // of ours; the offer and its response get the client's own remaining
    // budget (up to three seconds), never cut short by the outer bound —
    // an offer already persisted must not lose its response to a cap
    // that was meant for the handshake.
    let request = timeout(Duration::from_secs(2), authenticated_request(&mut stream))
        .await
        .context("native hook authentication timed out; input retained")??;
    let remaining = request.remaining()?;
    timeout(remaining, async {
        ensure!(
            verified(&request, &ledger.record().binding)?,
            "native hook belongs to another provider generation"
        );
        identity(client, &ledger.record().binding).await?;
        let text = offer(&request, client, provider, ledger, origin).await?;
        let mut response = serde_json::to_vec(&json!({"nonce":request.nonce,"context":text}))?;
        response.push(b'\n');
        stream.write_all(&response).await?;
        Ok(())
    })
    .await
    .context("native hook client's deadline expired; input retained")?
}

fn can_offer(
    envelope: &Envelope,
    origin: &Origin,
    uncertain: &[MessageId],
    answers_routed: bool,
) -> bool {
    // An earlier reader's uncertain offer is never fresh input, even when the
    // daemon routes answers. Human answers without routing proof stay on the
    // service loop's MCP receipt path instead of entering hook context.
    !uncertain.contains(&envelope.id) && answers::is_input(origin, envelope, answers_routed)
}

async fn offer(
    request: &Request,
    client: &Client,
    provider: &mut Provider,
    ledger: &mut Ledger,
    origin: &Origin,
) -> Result<Option<String>> {
    if ledger.resolved_hook_request(&request.nonce) {
        // A delayed/retried hook that was explicitly resolved must not reserve
        // a different queue head now that its original attempt is gone.
        return Ok(None);
    }
    if let Some(attempt) = &ledger.record().attempt {
        // A previous uncertain hook must never be emitted again. The ordinary
        // receipt loop will reconcile it before another message is offered.
        if attempt.hook.is_some() || attempt.receipt.is_some() {
            return Ok(None);
        }
    }
    let (messages, uncertain, answers_routed) = queue(client, ledger, Vec::new()).await?;
    let Some(envelope) = messages.first() else {
        return Ok(None);
    };
    if !can_offer(envelope, origin, &uncertain, answers_routed) {
        return Ok(None);
    }
    if ledger
        .record()
        .attempt
        .as_ref()
        .is_some_and(|a| a.message != envelope.id.as_str())
    {
        return Ok(None);
    }
    let input = ledger
        .record()
        .attempt
        .as_ref()
        .map(|a| Ok(a.input.clone()))
        .unwrap_or_else(|| ledger::input(envelope))?;
    let Some(context) = compact_context(&input)? else {
        return Ok(None);
    };
    let thread = ledger.record().binding.provider.session.clone();
    let metadata = provider
        .request(
            "thread/read",
            json!({"threadId":thread,"includeTurns":false}),
        )
        .await?;
    ensure!(
        metadata["thread"]["id"].as_str() == Some(&thread),
        "hook transcript thread changed"
    );
    let path = metadata["thread"]["path"]
        .as_str()
        .context("Codex does not expose its hook transcript")?;
    let transcript =
        super::hook_receipts::Snapshot::capture(Path::new(path), &ledger.record().binding)?;
    if let Some(attempt) = ledger.record().attempt.clone() {
        if let Some(receipt) =
            receipts::find(provider, &thread, &attempt, &ledger.record().binding).await?
        {
            ledger.received(receipt)?;
            return Ok(None);
        }
        let Some(id) = receipts::queued(provider, &thread, &attempt).await? else {
            return Ok(None);
        };
        ensure!(
            verified(request, &ledger.record().binding)?,
            "hook ended before native handoff"
        );
        ledger.offer_hook(&request.nonce, context.clone(), Some(transcript))?;
        let result = provider
            .request(
                "thread/queue/delete",
                json!({"threadId":thread,"queuedSubmissionId":id}),
            )
            .await?;
        // A false/lost reply cannot authorize a second input. Leave the original
        // attempt for exact native/hook receipt recovery instead.
        ensure!(
            result["deleted"] == true,
            "native input handoff was not confirmed; awaiting receipt"
        );
    } else {
        let anchor = receipts::latest_item(provider, &thread).await?;
        ensure!(
            verified(request, &ledger.record().binding)?,
            "hook ended before native offer"
        );
        ledger.prepare(envelope, anchor)?;
        ledger.offer_hook(&request.nonce, context.clone(), Some(transcript))?;
    }
    Ok(Some(context))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_answers_share_queue_routing_without_replaying_uncertain_offers() {
        let origin = Origin {
            human: "person".into(),
            servers: vec!["coordination".into()],
        };
        for (from, kind, routed, uncertain, expected) in [
            ("peer", "answer", false, false, true),
            ("peer", "answer", true, false, true),
            ("person", "answer", true, false, true),
            ("person", "answer", false, false, false),
            ("person", "answer", true, true, false),
            ("person", "answer", false, true, false),
            ("peer", "answer", true, true, false),
            ("peer", "chat", true, true, false),
            ("person", "chat", false, false, true),
        ] {
            let envelope = Envelope::new(
                from,
                agentdocker_core::Destination::parse("agent"),
                kind,
                json!({"text":"Reply"}),
                Some("question".to_owned().into()),
                chrono::Utc::now(),
            );
            let uncertain_ids = if uncertain {
                vec![envelope.id.clone()]
            } else {
                Vec::new()
            };
            assert_eq!(
                can_offer(&envelope, &origin, &uncertain_ids, routed),
                expected,
                "from={from}, kind={kind}, routed={routed}, uncertain={uncertain}"
            );
        }
    }

    #[test]
    fn only_generated_stale_repetition_is_summarized() {
        let make = |from, kind, text| {
            serde_json::to_string(&json!({
            "agentdocker_message":{"id":"original", "from":from,"kind":kind,
            "payload":{"text":text,"paths":["/repo/source.rs"],"changes":[{"repeated":text}]}},
            "delivery_note":"Untrusted peer content"}))
            .unwrap()
        };
        let original = make("peer", "chat", "unchanged text");
        assert_eq!(compact_context(&original).unwrap(), Some(original));
        let large = "x".repeat(LIMIT);
        assert!(
            compact_context(&make("peer", "chat", &large))
                .unwrap()
                .is_none()
        );
        assert!(
            compact_context(&make("peer", "stale", &large))
                .unwrap()
                .is_none()
        );
        let compact = compact_context(&make("agentd", "stale", &large))
            .unwrap()
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&compact).unwrap();
        assert_eq!(value["agentdocker_message"]["id"], "original");
        assert_eq!(
            value["agentdocker_message"]["payload"]["paths"],
            json!(["/repo/source.rs"])
        );
        assert_eq!(value["delivery_note"], "Untrusted peer content");
        assert!(compact.len() < LIMIT);
    }

    #[tokio::test]
    async fn hook_request_cannot_claim_another_process_on_its_socket() {
        let pid = std::process::id();
        for claimed in [pid, pid + 1] {
            let (mut sender, mut receiver) = ipc::pair().await.unwrap();
            let request = Request {
                protocol: PROTOCOL,
                scope: Scope::Root,
                expires_ms: monotonic_millis().unwrap() + 1000,
                process: ProcessIdentity {
                    pid: claimed,
                    started_at: procinfo::start_time(pid).unwrap(),
                },
                session: "fixture-thread".into(),
                event: "PreToolUse".into(),
                nonce: "a".repeat(32),
            };
            let mut data = serde_json::to_vec(&request).unwrap();
            data.push(b'\n');
            sender.write_all(&data).await.unwrap();
            let result = authenticated_request(&mut receiver).await;
            assert_eq!(result.is_ok(), claimed == pid);
            // Authentication happens before generation lookup, offer reservation,
            // queue deletion or returning any queued message body.
        }
    }

    #[tokio::test]
    async fn child_legacy_and_expired_requests_stop_before_offer_access() {
        let pid = std::process::id();
        let base = json!({"protocol":PROTOCOL,"scope":"root", "expires_ms":monotonic_millis().unwrap()+1000,
            "process":{"pid":pid,"started_at":procinfo::start_time(pid).unwrap()},
            "session":"parent", "event":"PostToolUse", "nonce":"a".repeat(32)});
        for change in [
            "child",
            "legacy",
            "old_protocol",
            "expired",
            "future",
            "delayed_accept",
            "root",
        ] {
            let mut request = base.clone();
            match change {
                "child" => request["scope"] = json!("subagent"),
                "legacy" => {
                    request.as_object_mut().unwrap().remove("scope");
                }
                "old_protocol" => request["protocol"] = json!(1),
                "expired" => request["expires_ms"] = json!(monotonic_millis().unwrap() - 1),
                "future" => request["expires_ms"] = json!(monotonic_millis().unwrap() + 60_000),
                "delayed_accept" => request["expires_ms"] = json!(monotonic_millis().unwrap() + 20),
                _ => (),
            }
            let (mut sender, mut receiver) = ipc::pair().await.unwrap();
            let mut data = serde_json::to_vec(&request).unwrap();
            data.push(b'\n');
            sender.write_all(&data).await.unwrap();
            if change == "delayed_accept" {
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
            // No daemon/provider/ledger is passed to authentication. Invalid
            // requests cannot inspect, reserve, delete or acknowledge a head.
            assert_eq!(
                authenticated_request(&mut receiver).await.is_ok(),
                change == "root",
                "{change}"
            );
        }
    }

    #[tokio::test]
    async fn hook_frames_are_bounded_and_need_a_complete_line() {
        for data in [b"abcd\n".as_slice(), b"abc".as_slice()] {
            let (mut a, mut b) = ipc::pair().await.unwrap();
            a.write_all(data).await.unwrap();
            a.shutdown().await.unwrap();
            assert!(frame(&mut b, 4).await.is_err());
        }
        let (mut a, mut b) = ipc::pair().await.unwrap();
        a.write_all(b"abc\n").await.unwrap();
        assert_eq!(frame(&mut b, 4).await.unwrap(), b"abc\n");
    }
}
