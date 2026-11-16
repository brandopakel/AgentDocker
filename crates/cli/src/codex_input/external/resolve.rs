//! Explicit recovery of retained input, bound to a reviewed provider generation.
//! Manual hook readback and starting an existing native queue entry use distinct
//! confirmations. History or elapsed time never silently authorizes either.
pub(super) use super::local::Listener;
use super::{
    Client, Ledger, Provider, call, identity,
    ledger::Phase,
    local, queued_start,
    receipts::{self, Scheduled},
    verify_provider,
};
use agentdocker_core::{ProcessIdentity, ProviderGeneration, Request, Response};
use agentdocker_host::{dirs, ipc, procinfo};
use anyhow::{Context, Result, bail, ensure};
use ipc::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

#[derive(clap::Args)]
pub struct Args {
    /// Exact Codex agent whose retained input should be reviewed.
    #[arg(long, env = "AGENTDOCKER_AGENT_ID")]
    pub agent: String,
    /// Message ID from the review. Omit action options for a read-only preview.
    #[arg(long, requires = "note")]
    pub message: Option<String>,
    /// Confirm that you read the complete message, using the preview's digest.
    #[arg(long, requires_all = ["message", "note"], conflicts_with = "start_queued")]
    pub confirm_read: Option<String>,
    /// Explicitly start the existing queued entry using its preview digest.
    #[arg(long, requires_all = ["message", "note"], conflicts_with = "confirm_read")]
    pub start_queued: Option<String>,
    /// Why this readback or queued start is requested (recorded in the journal).
    #[arg(long, requires = "message")]
    pub note: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Preview {
        agent: String,
        provider: ProviderGeneration,
        socket: PathBuf,
    },
    Confirm {
        agent: String,
        provider: ProviderGeneration,
        socket: PathBuf,
        message: String,
        confirmation: String,
        note: String,
    },
    StartQueued {
        agent: String,
        provider: ProviderGeneration,
        socket: PathBuf,
        message: String,
        confirmation: String,
        note: String,
    },
}

async fn frame(stream: &mut Stream, max: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    BufReader::new(stream.take((max + 1) as u64))
        .read_until(b'\n', &mut bytes)
        .await?;
    ensure!(
        bytes.len() <= max && bytes.last() == Some(&b'\n'),
        "invalid input recovery frame"
    );
    Ok(bytes)
}

async fn write(stream: &mut Stream, response: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(response)?;
    bytes.push(b'\n');
    stream.write_all(&bytes).await?;
    Ok(())
}

/// The readback's refusals that the review can foresee, in one place so
/// that what the review says and what the command answers are the same
/// words.
const NEEDS_JOURNAL: &str =
    "manual readback requires a project journal; retained input is unchanged";
const STILL_QUEUED: &str = "matching native input is still queued; manual acknowledgement refused";

/// What the review saw of the live queue.
#[derive(Debug)]
enum Look {
    /// The phase's next step does not depend on the queue: it was not read.
    NotMade,
    /// The whole queue was read, and this is what it holds of the input.
    Found(Scheduled),
    /// The read failed, did not finish, or contradicted itself. Never read
    /// as absence, and never grounds for either command.
    NotEstablished(String),
}

/// One look at the live queue, in the phases whose next step depends on
/// it. It writes nothing, and its failure is reported, not returned: what
/// is retained stays reviewable when the queue cannot be read.
async fn look(provider: &mut Provider, ledger: &Ledger) -> Look {
    let record = ledger.record();
    let Some(attempt) = &record.attempt else {
        return Look::NotMade;
    };
    match record.phase() {
        Phase::Queued | Phase::StartPrepared | Phase::HookOffered | Phase::ManualRead { .. } => {
            match receipts::look(provider, &record.binding.provider.session, attempt).await {
                Ok(seen) => Look::Found(seen),
                Err(error) => Look::NotEstablished(format!("{error:#}")),
            }
        }
        Phase::Empty
        | Phase::Prepared
        | Phase::StartTransmitted { .. }
        | Phase::SteerOffered
        | Phase::SteerAccepted
        | Phase::Received => Look::NotMade,
    }
}

const ELIGIBLE: &str = "eligible from the retained record and this look; the command repeats every live check itself and may refuse";
const UNREAD: &str = "not established: the live queue could not be read, so nothing is recommended";

/// What `--confirm-read` would meet now, as far as the review can tell.
fn readback_next(phase: Phase, project: bool, look: &Look) -> String {
    if !project {
        return format!("refused: {NEEDS_JOURNAL}");
    }
    if let Err(why) = phase.readback() {
        return format!("refused: {why}");
    }
    match look {
        // Any scheduled entry of the same identity blocks a readback, its
        // text edited or not.
        Look::Found(Scheduled::Recorded | Scheduled::Other) => format!("refused: {STILL_QUEUED}"),
        Look::Found(Scheduled::Absent) => match phase {
            Phase::ManualRead { .. } => {
                format!("{ELIGIBLE}. With the confirmation it was begun with, it resumes.")
            }
            _ => format!(
                "{ELIGIBLE}. Use `confirmation`, after reading the complete envelope back in the session."
            ),
        },
        Look::NotMade | Look::NotEstablished(_) => UNREAD.to_owned(),
    }
}

/// What `--start-queued` would meet now, as far as the review can tell.
fn start_next(phase: Phase, shared: bool, project: bool, look: &Look) -> String {
    if !shared {
        return format!("refused: {}", queued_start::NEEDS_SHARED_SERVER);
    }
    if let Phase::StartTransmitted { .. } = phase {
        return "reports only: with the intent's confirmation it answers that the start was already attempted, and starts nothing".to_owned();
    }
    if let Err(why) = phase.start() {
        return format!("refused: {why}");
    }
    if !project {
        return format!("refused: {}", queued_start::NEEDS_PROJECT);
    }
    match look {
        Look::Found(Scheduled::Recorded) => match phase {
            Phase::StartPrepared => format!(
                "{ELIGIBLE}. The explicit start has not begun transmission; its preflight may be retried with the same `start_confirmation`."
            ),
            _ => format!("{ELIGIBLE}. Use `start_confirmation`."),
        },
        // Only the recorded entry, unchanged, is started.
        Look::Found(Scheduled::Other | Scheduled::Absent) => {
            format!("refused: {}", queued_start::ENTRY_UNAVAILABLE)
        }
        Look::NotMade | Look::NotEstablished(_) => UNREAD.to_owned(),
    }
}

/// Where the input stands for the person, in a sentence: what it waits
/// on, and why it is not sent again. It never says a receipt is absent
/// (the review does not search for one) or that a turn ended.
fn standing(phase: Phase, look: &Look) -> &'static str {
    match (phase, look) {
        (Phase::Empty, _) => "Nothing is pending.",
        (Phase::Received, _) => {
            "A provider receipt is recorded. Nothing to do: the receiver acknowledges it."
        }
        (Phase::Prepared, _) => {
            "No queue entry and no receipt are recorded. Its add may have reached Codex, so it is not sent again; reconciliation may still find its entry or its receipt."
        }
        (Phase::StartTransmitted { .. }, _) => {
            "The explicit start may have reached Codex. It stays retained for its exact provider receipt and has no manual readback: a readback needs a hook offer, and a start excludes one."
        }
        (Phase::SteerOffered, _) => {
            "Recorded before its steer was written, with no answer recorded. It may have reached Codex; it stays retained for its exact receipt in that turn."
        }
        (Phase::SteerAccepted, _) => {
            "The server accepted the steer, which is not proof the session received it. It stays retained for the exact receipt in the turn it was steered into."
        }
        (_, Look::NotMade | Look::NotEstablished(_)) => {
            "The live queue could not be read. What is retained is shown as it is, and nothing is recommended."
        }
        (Phase::Queued, Look::Found(Scheduled::Recorded)) => {
            "Codex holds its queue entry, unchanged, and no receipt is recorded."
        }
        (Phase::StartPrepared, Look::Found(Scheduled::Recorded)) => {
            "An explicit start is recorded and has not begun transmission. Codex holds its queue entry, unchanged."
        }
        (Phase::Queued | Phase::StartPrepared, Look::Found(Scheduled::Other)) => {
            "A matching entry is scheduled, but it is not the recorded one. No receipt is recorded; reconciliation may still find one."
        }
        (Phase::Queued | Phase::StartPrepared, Look::Found(Scheduled::Absent)) => {
            "The recorded entry is not in the queue and no receipt is recorded. It may have run, so it is not sent again; reconciliation may still find its receipt."
        }
        (
            Phase::HookOffered | Phase::ManualRead { .. },
            Look::Found(Scheduled::Recorded | Scheduled::Other),
        ) => {
            "A matching native entry is still scheduled; Codex may yet run it, and a readback waits until it is gone."
        }
        (Phase::HookOffered, Look::Found(Scheduled::Absent)) => {
            "Offered through a hook, with no receipt recorded and nothing matching scheduled. Whether the session received it is for whoever reads it back to say."
        }
        (Phase::ManualRead { .. }, Look::Found(Scheduled::Absent)) => {
            "A manual readback is recorded and not finished; nothing matching is scheduled."
        }
    }
}

/// What the look found, in words; `null` where it was not made.
fn queue_words(look: &Look) -> Value {
    match look {
        Look::NotMade => Value::Null,
        Look::Found(Scheduled::Recorded) => "the recorded entry, unchanged".into(),
        Look::Found(Scheduled::Other) => {
            "a matching entry that is not the recorded one (edited, or under another id)".into()
        }
        Look::Found(Scheduled::Absent) => "no matching entry".into(),
        Look::NotEstablished(why) => format!("not established: {why}").into(),
    }
}

/// What is retained, for the person to review. `phase`, `queue` and
/// `next` are derived for the review; every other key is as it has been.
fn preview(ledger: &Ledger, project: bool, look: &Look) -> Result<Value> {
    let record = ledger.record();
    let phase = record.phase();
    let shared = record.binding.remote.is_some();
    let pending = record
        .attempt
        .as_ref()
        .map(|attempt| -> Result<Value> {
            Ok(json!({
                "message": attempt.message,
                "envelope": serde_json::from_str::<Value>(&attempt.input)?["agentdocker_message"],
                "confirmation": ledger.confirmation()?,
                "hook_offer": attempt.hook.is_some(),
                "native_receipt": attempt.receipt,
                "manual_read": ledger.pending_manual_read(),
                "queued_submission": attempt.queued,
                "start_confirmation": if attempt.start.as_ref().is_none_or(|s| !s.may_have_transmitted()) { ledger.start_confirmation().ok() } else { None },
                "start_intent": attempt.start,
                "steered": attempt.steer,
                "phase": phase.word(),
                "queue": queue_words(look),
                "next": {
                    "standing": standing(phase, look),
                    "confirm_read": readback_next(phase, project, look),
                    "start_queued": start_next(phase, shared, project, look),
                },
            }))
        })
        .transpose()?;
    Ok(
        json!({"agent":record.binding.agent,"provider":record.binding.provider,"pending":pending,
        "notice":"Review the complete envelope. --confirm-read records manual readback of a hook offer; --start-queued explicitly starts the existing native queue entry. Neither creates a replacement input, and a start reply is not a delivery receipt."}),
    )
}

/// Continue a durable disposition after a lost journal/ACK response or restart.
/// Repeated journal writes carry the same resolution ID; a lost response can
/// duplicate that note, but cannot turn into a native receipt or another input.
pub(super) async fn reconcile(
    client: &Client,
    provider: &mut Provider,
    ledger: &mut Ledger,
) -> Result<()> {
    ensure!(
        ledger.pending_manual_read().is_some(),
        "no manual readback is pending"
    );
    identity(client, &ledger.record().binding).await?;
    let attempt = ledger
        .record()
        .attempt
        .as_ref()
        .context("manual readback lost its retained input")?;
    ensure!(
        receipts::queued_for_manual(provider, &ledger.record().binding.provider.session, attempt)
            .await?
            .is_none(),
        STILL_QUEUED
    );
    record_and_ack(client, ledger).await
}

async fn record_and_ack(client: &Client, ledger: &mut Ledger) -> Result<()> {
    let read = ledger
        .pending_manual_read()
        .cloned()
        .context("no manual readback is pending")?;
    if !read.journaled {
        let summary = format!(
            "Manual input readback {}: message {}, input SHA-256 {}, hook {}; operator pid {} born {}, provider pid {} / thread {}. {}. This is explicit readback, not a native provider receipt; no resubmission.",
            read.id,
            read.message,
            read.input_sha256,
            read.hook_request,
            read.operator.pid,
            read.operator.started_at,
            read.provider.process.pid,
            read.provider.session,
            read.note
        );
        ensure!(
            matches!(
                call(
                    client,
                    Request::JournalAdd {
                        agent: ledger.record().binding.agent.clone(),
                        summary
                    }
                )
                .await?,
                Response::JournalEntry { .. }
            ),
            "manual readback journal write was not confirmed"
        );
        ledger.manual_journaled(&read.id)?;
    }
    // ProviderInbox checks the current binding token and acknowledges idempotently.
    // It does not require (and this path does not invent) InputReport::Received.
    super::queue(client, ledger, vec![read.message.into()]).await?;
    ledger.acknowledge_manual(&read.id)
}

async fn handle(
    command: Command,
    operator: ProcessIdentity,
    client: &Client,
    provider: &mut Provider,
    ledger: &mut Ledger,
) -> Result<Value> {
    let (agent, expected, socket) = match &command {
        Command::Preview {
            agent,
            provider,
            socket,
        }
        | Command::Confirm {
            agent,
            provider,
            socket,
            ..
        }
        | Command::StartQueued {
            agent,
            provider,
            socket,
            ..
        } => (agent, provider, socket),
    };
    ensure!(
        agent == &ledger.record().binding.agent
            && expected == &ledger.record().binding.provider
            && socket == &ledger.record().binding.socket,
        "recovery request names another agent, provider generation or daemon"
    );
    let current = identity(client, &ledger.record().binding).await?;
    match command {
        Command::Preview { .. } => {
            let look = look(provider, ledger).await;
            preview(ledger, current.project.is_some(), &look)
        }
        Command::StartQueued {
            message,
            confirmation,
            note,
            ..
        } => {
            super::queued_start::start(
                client,
                provider,
                ledger,
                &message,
                &confirmation,
                &note,
                operator,
            )
            .await
        }
        Command::Confirm {
            message,
            confirmation,
            note,
            ..
        } => {
            ensure!(current.project.is_some(), NEEDS_JOURNAL);
            if let Some(read) = ledger
                .record()
                .manual_reads
                .iter()
                .find(|r| r.message == message && r.confirmation == confirmation)
            {
                if read.acknowledged {
                    return Ok(
                        json!({"manually_acknowledged":message,"resolution":read.id,"already_applied":true}),
                    );
                }
            } else {
                let attempt = ledger
                    .record()
                    .attempt
                    .as_ref()
                    .context("no retained input to resolve")?;
                ensure!(
                    attempt.message == message && ledger.confirmation()? == confirmation,
                    "retained input or provider generation changed; review it again"
                );
                // What the phase alone refuses, in the words the review
                // gave, before the queue is read for nothing.
                ledger.phase().readback().map_err(anyhow::Error::msg)?;
                ensure!(
                    receipts::queued_for_manual(
                        provider,
                        &ledger.record().binding.provider.session,
                        attempt
                    )
                    .await?
                    .is_none(),
                    STILL_QUEUED
                );
                ledger.begin_manual_read(&message, &confirmation, &note, operator)?;
            }
            let id = ledger
                .pending_manual_read()
                .context("no manual readback to reconcile")?
                .id
                .clone();
            reconcile(client, provider, ledger).await?;
            Ok(json!({"manually_acknowledged":message,"resolution":id,"already_applied":false}))
        }
    }
}

pub(super) async fn serve(
    mut stream: Stream,
    client: &Client,
    provider: &mut Provider,
    ledger: &mut Ledger,
) -> Result<()> {
    let result = tokio::time::timeout(Duration::from_secs(75), async {
        // Explicit local administration remains bound to the same-user kernel peer.
        let pid = ipc::peer_pid(&stream)?;
        let operator = ProcessIdentity {
            pid,
            started_at: procinfo::start_time(pid).context("input recovery peer exited")?,
        };
        let command =
            tokio::time::timeout(Duration::from_secs(2), frame(&mut stream, 16 * 1024)).await??;
        handle(
            serde_json::from_slice(&command)?,
            operator,
            client,
            provider,
            ledger,
        )
        .await
    })
    .await;
    let response = match result {
        Ok(Ok(value)) => json!({"ok":value}),
        Ok(Err(error)) => json!({"error":format!("{error:#}")}),
        Err(_) => {
            json!({"error":"input recovery timed out; the disposition is retained; retry the same confirmation"})
        }
    };
    tokio::time::timeout(Duration::from_secs(2), write(&mut stream, &response)).await??;
    Ok(())
}

/// The recovery endpoint must remain usable while ordinary delivery is paused.
pub(super) async fn while_paused(
    listener: &Listener,
    client: &Client,
    ledger: &mut Ledger,
) -> Result<()> {
    let process = ledger.record().binding.provider.process.clone();
    let provider_exit = async {
        while procinfo::start_time(process.pid) == Some(process.started_at) {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    };
    let recovery = async {
        tokio::select! {
            stream = listener.accept() => {
                let stream = stream?;
                let binding = ledger.record().binding.clone();
                let mut provider = super::connect_provider(&binding).await?;
                let result = async {
                    verify_provider(&mut provider, &binding).await?;
                    serve(stream, client, &mut provider, ledger).await
                }.await;
                let shutdown = provider.shutdown().await;
                result.and(shutdown)
            }
            _ = tokio::time::sleep(Duration::from_secs(30)) => Ok(()),
        }
    };
    // A daemon-restarted receiver is outside the native launcher's process
    // tree. Provider death must interrupt both the reconnect delay and any
    // recovery client wait. Durable intents already fence uncertain writes;
    // dropping this observer never acknowledges or retries retained input.
    tokio::select! {
        biased;
        _ = provider_exit => Ok(()),
        result = recovery => result,
    }
}

pub async fn run(client: Client, args: Args) -> Result<()> {
    let client = client.with_start_timeout(None);
    let Response::Agent { agent } = call(&client, Request::Inspect { agent: args.agent }).await?
    else {
        bail!("Codex agent is unavailable");
    };
    ensure!(
        agent.spec.runtime == "codex" && !agent.managed && agent.input_binding.is_some(),
        "manual recovery requires an existing Codex native receiver"
    );
    let path = local::endpoint(&dirs::home(), agent.id.as_str(), "resolve")?;
    let mut stream = Stream::connect(path).await.context(
        "receiver has no recovery endpoint; upgrade the receiver before resolving retained input",
    )?;
    let accepted = agent
        .input_binding
        .as_ref()
        .context("receiver binding disappeared")?;
    let peer = ipc::peer_pid(&stream)?;
    ensure!(
        peer == accepted.controller.pid
            && procinfo::start_time(accepted.controller.pid)
                == Some(accepted.controller.started_at),
        "recovery endpoint is not owned by the bound receiver"
    );
    let command = match (
        args.message,
        args.confirm_read,
        args.start_queued,
        args.note,
    ) {
        (Some(message), Some(confirmation), None, Some(note)) => Command::Confirm {
            agent: agent.id.to_string(),
            provider: accepted.provider.clone(),
            socket: client.socket_path().into(),
            message,
            confirmation,
            note,
        },
        (Some(message), None, Some(confirmation), Some(note)) => Command::StartQueued {
            agent: agent.id.to_string(),
            provider: accepted.provider.clone(),
            socket: client.socket_path().into(),
            message,
            confirmation,
            note,
        },
        (None, None, None, None) => Command::Preview {
            agent: agent.id.to_string(),
            provider: accepted.provider.clone(),
            socket: client.socket_path().into(),
        },
        _ => bail!("use --message and --note with exactly one of --confirm-read or --start-queued"),
    };
    tokio::time::timeout(Duration::from_secs(90), async {
        write(&mut stream, &serde_json::to_value(command)?).await?;
        let response: Value = serde_json::from_slice(&frame(&mut stream, 2 * 1024 * 1024).await?)?;
        if let Some(error) = response["error"].as_str() {
            bail!("{error}");
        }
        let value = response.get("ok").context("invalid recovery response")?;
        // A preview is a report and prints as one. A confirmation creates a
        // resolution or start intent: its id alone goes to stdout, as every command that
        // creates something prints its id, and the rest of the report goes
        // to stderr for the person reading.
        match value
            .get("resolution")
            .or_else(|| value.get("queued_start"))
            .and_then(Value::as_str)
        {
            Some(resolution) => {
                eprintln!("{}", serde_json::to_string_pretty(value)?);
                println!("{resolution}");
            }
            None => println!("{}", serde_json::to_string_pretty(value)?),
        }
        Ok(())
    })
    .await
    .context("receiver did not answer; input and any pending disposition are retained")?
}

#[cfg(test)]
mod tests {
    use super::super::{ledger, testing};
    use super::*;
    use agentdocker_core::{Destination, Envelope};

    #[tokio::test]
    async fn provider_exit_interrupts_paused_recovery_without_changing_retained_input() {
        #[cfg(unix)]
        let mut command = tokio::process::Command::new("/bin/sleep");
        #[cfg(unix)]
        command.arg("60");
        #[cfg(windows)]
        let mut command = tokio::process::Command::new("powershell.exe");
        #[cfg(windows)]
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 60",
        ]);
        let mut child = command.kill_on_drop(true).spawn().unwrap();
        let pid = child.id().unwrap();
        #[cfg(unix)]
        let root = tempfile::tempdir_in("/tmp").unwrap();
        #[cfg(windows)]
        let root = tempfile::tempdir().unwrap();
        let home = root.path().canonicalize().unwrap();
        let binding = ledger::Binding {
            agent: "paused-exit".into(),
            provider: ProviderGeneration {
                process: ProcessIdentity {
                    pid,
                    started_at: procinfo::start_time(pid).unwrap(),
                },
                session: "thread".into(),
                profile: home.to_string_lossy().into(),
            },
            socket: home.join("unused-daemon"),
            cwd: home.clone(),
            executable: home.join("codex"),
            remote: None,
            control: false,
        };
        let mut ledger = Ledger::open(&home, binding.clone(), None).unwrap();
        let message = Envelope::new(
            "peer",
            Destination::parse("paused-exit"),
            "chat",
            json!({"text":"retain this uncertain input"}),
            None,
            chrono::Utc::now(),
        );
        ledger.prepare(&message, None).unwrap();
        ledger.queued("existing-queue-entry").unwrap();
        let before = serde_json::to_vec(ledger.record()).unwrap();
        let path = ledger::directory(&home, "paused-exit")
            .unwrap()
            .join("delivery.json");
        let persisted = std::fs::read(&path).unwrap();
        let listener = Listener::bind(&home, "paused-exit", "resolve").unwrap();
        let client = Client::new(Some(binding.socket)).with_start_timeout(None);
        {
            let recovery = while_paused(&listener, &client, &mut ledger);
            tokio::pin!(recovery);
            assert!(
                tokio::time::timeout(Duration::from_millis(50), &mut recovery)
                    .await
                    .is_err()
            );
            child.kill().await.unwrap();
            child.wait().await.unwrap();
            tokio::time::timeout(Duration::from_secs(3), &mut recovery)
                .await
                .unwrap()
                .unwrap();
        }
        assert_eq!(serde_json::to_vec(ledger.record()).unwrap(), before);
        assert_eq!(std::fs::read(path).unwrap(), persisted);
    }

    #[tokio::test]
    async fn lost_journal_and_ack_replies_reconcile_the_same_manual_disposition() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().canonicalize().unwrap();
        let socket = home.join("sock");
        let binding = ledger::Binding {
            agent: "agent".into(),
            provider: ProviderGeneration {
                process: ProcessIdentity {
                    pid: 1,
                    started_at: chrono::Utc::now(),
                },
                session: "thread".into(),
                profile: home.to_string_lossy().into(),
            },
            socket: socket.clone(),
            cwd: home.clone(),
            executable: home.join("codex"),
            remote: None,
            control: false,
        };
        let message = Envelope::new(
            "peer",
            Destination::parse("agent"),
            "chat",
            json!({"text":"manually reviewed"}),
            None,
            chrono::Utc::now(),
        );
        let mut ledger = Ledger::open(&home, binding.clone(), None).unwrap();
        ledger.prepare(&message, None).unwrap();
        ledger
            .offer_hook("original-hook", ledger::input(&message).unwrap(), None)
            .unwrap();
        let confirmation = ledger.confirmation().unwrap();
        ledger
            .begin_manual_read(
                message.id.as_str(),
                &confirmation,
                "read the full original",
                ProcessIdentity {
                    pid: 2,
                    started_at: chrono::Utc::now(),
                },
            )
            .unwrap();
        let resolution = ledger.pending_manual_read().unwrap().id.clone();
        let token = ledger.record().token.clone();
        let id = message.id.clone();
        let listener = ipc::Listener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let mut notes = Vec::new();
            for step in 0..4 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                let response = match serde_json::from_str::<Request>(&line).unwrap() {
                    Request::JournalAdd { agent, summary } if step < 2 => {
                        assert_eq!(agent, "agent");
                        notes.push(summary.clone());
                        Response::JournalEntry {
                            entry: serde_json::from_value(json!({
                                "project":"project","seq":step+1,"at":chrono::Utc::now(),
                                "agent":"agent","agent_name":"agent","kind":"note",
                                "summary":summary,"summary_source":"explicit"
                            }))
                            .unwrap(),
                        }
                    }
                    Request::ProviderInbox {
                        agent,
                        acknowledge,
                        token: observed,
                    } if step >= 2 => {
                        assert_eq!(agent, "agent");
                        assert_eq!(observed.as_ref(), Some(&token));
                        assert_eq!(acknowledge, vec![id.clone()]);
                        Response::InputBatch {
                            agent: agent.into(),
                            messages: vec![],
                            uncertain: vec![],
                            answers_routed: true,
                        }
                    }
                    other => panic!("unexpected recovery wire request: {other:?}"),
                };
                // Model a committed operation whose successful reply was lost.
                if step == 0 || step == 2 {
                    continue;
                }
                let mut line = serde_json::to_vec(&response).unwrap();
                line.push(b'\n');
                stream.get_mut().write_all(&line).await.unwrap();
            }
            notes
        });
        let client = Client::new(Some(socket)).with_start_timeout(None);
        assert!(record_and_ack(&client, &mut ledger).await.is_err());
        assert!(!ledger.pending_manual_read().unwrap().journaled);
        drop(ledger);
        let mut ledger = Ledger::open(&home, binding.clone(), None).unwrap();
        assert!(record_and_ack(&client, &mut ledger).await.is_err());
        assert!(ledger.pending_manual_read().unwrap().journaled);
        assert_eq!(
            ledger.record().attempt.as_ref().unwrap().message,
            message.id.as_str()
        );
        drop(ledger);
        let mut ledger = Ledger::open(&home, binding, None).unwrap();
        record_and_ack(&client, &mut ledger).await.unwrap();
        assert!(ledger.record().attempt.is_none());
        assert!(ledger.latest_receipt().is_none());
        assert!(ledger.prepare(&message, None).is_err());
        assert!(ledger.resolved_hook_request("original-hook"));
        assert_eq!(ledger.record().manual_reads.len(), 1);
        let notes = server.await.unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0], notes[1]);
        assert!(notes[0].contains(&resolution));
        assert!(notes[0].contains("not a native provider receipt"));
    }

    const NOTICE: &str = "Review the complete envelope. --confirm-read records manual readback of a hook offer; --start-queued explicitly starts the existing native queue entry. Neither creates a replacement input, and a start reply is not a delivery receipt.";

    fn bound(home: &std::path::Path, phase: Phase, shared: bool) -> ledger::Binding {
        ledger::Binding {
            agent: "agent".into(),
            provider: ProviderGeneration {
                process: ProcessIdentity {
                    pid: 1,
                    started_at: chrono::Utc::now(),
                },
                session: "thread".into(),
                profile: home.to_string_lossy().into(),
            },
            socket: home.join("sock"),
            cwd: home.into(),
            executable: home.join("codex"),
            remote: shared.then(|| super::super::remote::Descriptor {
                record: home.join("server.json"),
                sha256: "a".repeat(64),
            }),
            control: testing::steered(phase),
        }
    }

    fn looks() -> Vec<Look> {
        vec![
            Look::NotMade,
            Look::Found(Scheduled::Recorded),
            Look::Found(Scheduled::Other),
            Look::Found(Scheduled::Absent),
            Look::NotEstablished("the queue read failed".into()),
        ]
    }

    /// The review gains `phase`, `queue` and `next`, and every key it had
    /// keeps its value in every phase and whatever the queue shows: the
    /// scripts that read it read those. `start_confirmation` among them,
    /// shown as before where a start would be refused too.
    #[test]
    fn the_review_adds_three_keys_and_keeps_every_other_as_it_was() {
        use testing::{PHASES, take_to};
        for phase in PHASES {
            for shared in [false, true] {
                if shared && testing::steered(phase) {
                    continue;
                }
                let root = tempfile::tempdir().unwrap();
                let home = root.path().canonicalize().unwrap();
                let mut ledger = Ledger::open(&home, bound(&home, phase, shared), None).unwrap();
                let message = Envelope::new(
                    "peer",
                    Destination::parse("agent"),
                    "chat",
                    json!({"text":"review this"}),
                    None,
                    chrono::Utc::now(),
                );
                take_to(&mut ledger, &message, phase);
                for look in looks() {
                    let shown = preview(&ledger, true, &look).unwrap();
                    assert_eq!(shown["notice"], NOTICE);
                    assert_eq!(shown["agent"], "agent");
                    if phase == Phase::Empty {
                        assert!(shown["pending"].is_null());
                        continue;
                    }
                    let pending = shown["pending"].as_object().unwrap();
                    let mut keys: Vec<_> = pending.keys().map(String::as_str).collect();
                    keys.sort_unstable();
                    assert_eq!(
                        keys,
                        [
                            "confirmation",
                            "envelope",
                            "hook_offer",
                            "manual_read",
                            "message",
                            "native_receipt",
                            "next",
                            "phase",
                            "queue",
                            "queued_submission",
                            "start_confirmation",
                            "start_intent",
                            "steered",
                        ]
                    );
                    let attempt = ledger.record().attempt.as_ref().unwrap();
                    assert_eq!(pending["message"], message.id.as_str());
                    assert_eq!(pending["envelope"]["id"], message.id.as_str());
                    assert_eq!(pending["confirmation"], ledger.confirmation().unwrap());
                    assert_eq!(pending["hook_offer"], attempt.hook.is_some());
                    assert_eq!(
                        pending["native_receipt"],
                        serde_json::to_value(&attempt.receipt).unwrap()
                    );
                    assert_eq!(
                        pending["manual_read"],
                        serde_json::to_value(ledger.pending_manual_read()).unwrap()
                    );
                    assert_eq!(
                        pending["queued_submission"],
                        serde_json::to_value(&attempt.queued).unwrap()
                    );
                    assert_eq!(
                        pending["start_intent"],
                        serde_json::to_value(&attempt.start).unwrap()
                    );
                    assert_eq!(
                        pending["steered"],
                        serde_json::to_value(&attempt.steer).unwrap()
                    );
                    // As it was: a start's digest wherever the entry is
                    // recorded with no hook offer, no receipt and no start
                    // that may have been transmitted; the shared server
                    // and the live queue never entered into it.
                    let digest = matches!(phase, Phase::Queued | Phase::StartPrepared);
                    assert_eq!(
                        pending["start_confirmation"].is_string(),
                        digest,
                        "{phase:?}"
                    );
                    if digest {
                        assert_eq!(
                            pending["start_confirmation"],
                            ledger.start_confirmation().unwrap()
                        );
                    }
                    assert_eq!(pending["phase"], phase.word());
                    let next = pending["next"].to_string();
                    assert!(
                        !next.contains("was found")
                            && !next.contains("turn ended")
                            && !next.contains("has ended"),
                        "the review searches for no receipt and watches no turn: {next}"
                    );
                }
            }
        }
    }

    /// What each command would meet, phase by phase and by what the queue
    /// shows: an eligible request is said to be subject to the command's
    /// own checks, a blocker is said in the command's words, and a queue
    /// that could not be read recommends nothing.
    #[test]
    fn the_review_says_what_each_command_would_meet() {
        use testing::take_to;
        let next = |phase: Phase, shared: bool, project: bool, look: Look| {
            let root = tempfile::tempdir().unwrap();
            let home = root.path().canonicalize().unwrap();
            let mut ledger = Ledger::open(&home, bound(&home, phase, shared), None).unwrap();
            let message = Envelope::new(
                "peer",
                Destination::parse("agent"),
                "chat",
                json!({"text":"review this"}),
                None,
                chrono::Utc::now(),
            );
            take_to(&mut ledger, &message, phase);
            let pending = preview(&ledger, project, &look).unwrap()["pending"].take();
            (
                pending["next"]["confirm_read"].as_str().unwrap().to_owned(),
                pending["next"]["start_queued"].as_str().unwrap().to_owned(),
                pending["next"]["standing"].as_str().unwrap().to_owned(),
                pending["queue"].clone(),
            )
        };
        let found = |seen| Look::Found(seen);
        let no_hook = "refused: manual readback requires a retained hook offer";
        let unavailable = "refused: the original native queue entry is unavailable";
        let still =
            "refused: matching native input is still queued; manual acknowledgement refused";
        let not_shared = "refused: explicit start requires the launcher-owned shared native server";
        let qualified = "the command repeats every live check itself and may refuse";

        // Prepared: nothing looked at, nothing offered.
        let (read, start, standing, queue) = next(Phase::Prepared, true, true, Look::NotMade);
        assert_eq!(read, no_hook);
        assert_eq!(start, "refused: no verified native queue entry");
        assert!(standing.contains("not sent again") && standing.contains("may still find"));
        assert!(queue.is_null());

        // Queued: a start is eligible only on the recorded entry and the
        // shared server; anything else is its blocker.
        let (read, start, _, queue) = next(Phase::Queued, true, true, found(Scheduled::Recorded));
        assert_eq!(read, no_hook);
        assert!(
            start.starts_with("eligible") && start.contains(qualified),
            "{start}"
        );
        assert_eq!(queue, "the recorded entry, unchanged");
        for seen in [Scheduled::Other, Scheduled::Absent] {
            let (_, start, standing, _) = next(Phase::Queued, true, true, found(seen));
            assert_eq!(start, unavailable);
            assert!(
                standing.contains("reconciliation may still find"),
                "{standing}"
            );
        }
        let (_, start, _, _) = next(Phase::Queued, false, true, found(Scheduled::Recorded));
        assert_eq!(start, not_shared);
        let (_, start, _, _) = next(Phase::Queued, true, false, found(Scheduled::Recorded));
        assert_eq!(start, "refused: queued start requires a project journal");

        // A prepared start takes the same conditions: with its entry gone
        // or edited it is a blocker, not a retry presented as available.
        let (_, start, _, _) = next(Phase::StartPrepared, true, true, found(Scheduled::Recorded));
        assert!(
            start.starts_with("eligible")
                && start.contains(qualified)
                && start.contains("has not begun transmission"),
            "{start}"
        );
        assert!(!start.contains("never sent") && !start.contains("nothing was written"));
        for seen in [Scheduled::Other, Scheduled::Absent] {
            let (_, start, _, _) = next(Phase::StartPrepared, true, true, found(seen));
            assert_eq!(start, unavailable);
        }

        // A start that may have been transmitted has no way out here.
        for replied in [false, true] {
            let (read, start, standing, queue) = next(
                Phase::StartTransmitted { replied },
                true,
                true,
                Look::NotMade,
            );
            assert_eq!(read, no_hook);
            assert!(start.starts_with("reports only"), "{start}");
            assert!(standing.contains("has no manual readback"), "{standing}");
            assert!(queue.is_null());
        }

        // A hook offer may be read back unless something matching is
        // still scheduled, its text edited or not.
        let (read, start, _, _) = next(Phase::HookOffered, true, true, found(Scheduled::Absent));
        assert!(
            read.starts_with("eligible") && read.contains(qualified),
            "{read}"
        );
        assert_eq!(start, "refused: input already offered or received");
        for seen in [Scheduled::Recorded, Scheduled::Other] {
            let (read, _, _, _) = next(Phase::HookOffered, true, true, found(seen));
            assert_eq!(read, still);
        }
        let (read, _, _, _) = next(Phase::HookOffered, true, false, found(Scheduled::Absent));
        assert_eq!(
            read,
            "refused: manual readback requires a project journal; retained input is unchanged"
        );

        // A readback already begun takes the same condition.
        for journaled in [false, true] {
            let phase = Phase::ManualRead { journaled };
            let (read, _, _, _) = next(phase, true, true, found(Scheduled::Other));
            assert_eq!(read, still);
            let (read, _, _, _) = next(phase, true, true, found(Scheduled::Absent));
            assert!(
                read.starts_with("eligible") && read.contains("resumes"),
                "{read}"
            );
        }

        // A steer: accepted is not received, and neither command applies.
        let (read, start, standing, _) = next(Phase::SteerOffered, false, true, Look::NotMade);
        assert_eq!((read.as_str(), start.as_str()), (no_hook, not_shared));
        assert!(standing.contains("may have reached Codex"));
        let (_, _, standing, _) = next(Phase::SteerAccepted, false, true, Look::NotMade);
        assert!(standing.contains("not proof the session received it"));

        // Received: nothing to do.
        let (read, start, standing, _) = next(Phase::Received, true, true, Look::NotMade);
        assert_eq!(
            read,
            "refused: native receipt already exists; let the receiver reconcile it"
        );
        assert_eq!(start, "refused: input already offered or received");
        assert!(standing.contains("Nothing to do"));

        // A queue that could not be read: said, and nothing recommended,
        // in every phase that looks.
        for phase in [
            Phase::Queued,
            Phase::StartPrepared,
            Phase::HookOffered,
            Phase::ManualRead { journaled: false },
        ] {
            let (read, start, standing, queue) = next(
                phase,
                true,
                true,
                Look::NotEstablished("the queue read failed".into()),
            );
            assert!(!read.starts_with("eligible") && !start.starts_with("eligible"));
            assert!(
                read.starts_with("not established") || start.starts_with("not established"),
                "{phase:?}: {read} / {start}"
            );
            assert!(standing.contains("could not be read"));
            assert_eq!(queue, "not established: the queue read failed");
        }
    }

    /// The review and the two commands through the receiver's own handler,
    /// against a daemon and a Codex server that record what they are asked.
    #[cfg(unix)]
    mod through_the_handler {
        use super::super::*;
        use crate::codex_input::external::ledger;
        use crate::codex_input::external::testing::{
            self, FakeProvider, PHASES, World, daemon, provider, queue_entry, queue_page, take_to,
            world,
        };
        use agentdocker_core::{Envelope, Request};

        const LOOKING: [Phase; 5] = [
            Phase::Queued,
            Phase::StartPrepared,
            Phase::HookOffered,
            Phase::ManualRead { journaled: false },
            Phase::ManualRead { journaled: true },
        ];

        /// A provider whose queue holds `entries` and whose history is
        /// empty; any other request is a test's mistake.
        async fn codex(entries: Vec<Value>) -> FakeProvider {
            provider(move |method, _| match method {
                "thread/queue/list" => Ok(queue_page(&entries)),
                other => panic!("the provider was asked for {other}"),
            })
            .await
        }

        fn client(world: &World) -> Client {
            Client::new(Some(world.binding.socket.clone())).with_start_timeout(None)
        }

        async fn ask(
            world: &World,
            ledger: &mut Ledger,
            fake: &mut FakeProvider,
            action: Option<(&str, &Envelope, &str)>,
        ) -> Result<Value> {
            let (agent, provider, socket) = (
                world.binding.agent.clone(),
                world.binding.provider.clone(),
                world.binding.socket.clone(),
            );
            let command = match action {
                None => Command::Preview {
                    agent,
                    provider,
                    socket,
                },
                Some(("read", message, confirmation)) => Command::Confirm {
                    agent,
                    provider,
                    socket,
                    message: message.id.to_string(),
                    confirmation: confirmation.into(),
                    note: "asked in a test".into(),
                },
                Some((_, message, confirmation)) => Command::StartQueued {
                    agent,
                    provider,
                    socket,
                    message: message.id.to_string(),
                    confirmation: confirmation.into(),
                    note: "asked in a test".into(),
                },
            };
            handle(
                command,
                testing::operator(),
                &client(world),
                &mut fake.provider,
                ledger,
            )
            .await
        }

        fn kinds(seen: &testing::Seen<Request>) -> Vec<&'static str> {
            seen.lock()
                .unwrap()
                .iter()
                .map(|request| match request {
                    Request::Inspect { .. } => "inspect",
                    Request::JournalAdd { .. } => "journal",
                    Request::ProviderInbox { .. } => "inbox",
                    _ => "other",
                })
                .collect()
        }

        /// The review reads the queue once, and only in the phases whose
        /// next step depends on it; it asks the daemon who the session is
        /// and nothing else; and the ledger's file is the same bytes after
        /// it, the queue id it saw not recorded.
        #[tokio::test]
        async fn the_review_looks_only_where_it_must_and_writes_nothing() {
            for phase in PHASES {
                let world = world(false, testing::steered(phase), true);
                let mut ledger = world.ledger();
                let message = world.message("review this");
                take_to(&mut ledger, &message, phase);
                let asked = daemon(&world.binding.socket, world.agent.clone());
                // A scheduled entry for this very message, under an id
                // the ledger may not have recorded.
                let text = ledger::input(&message).unwrap();
                let mut fake =
                    codex(vec![queue_entry("seen-now", message.id.as_str(), &text)]).await;
                let before = world.stored();
                let record = serde_json::to_vec(ledger.record()).unwrap();
                let shown = ask(&world, &mut ledger, &mut fake, None).await.unwrap();
                assert_eq!(world.stored(), before, "{phase:?}");
                assert_eq!(serde_json::to_vec(ledger.record()).unwrap(), record);
                assert_eq!(kinds(&asked), ["inspect"], "{phase:?}");
                let looked = LOOKING.contains(&phase);
                assert_eq!(
                    fake.methods(),
                    if looked {
                        vec!["thread/queue/list".to_owned()]
                    } else {
                        Vec::new()
                    },
                    "{phase:?}"
                );
                if phase != Phase::Empty {
                    assert_eq!(shown["pending"]["queue"].is_null(), !looked, "{phase:?}");
                    assert_eq!(shown["pending"]["phase"], phase.word());
                }
            }
        }

        /// A queue the provider will not list is said so. The phase, the
        /// envelope and the digests are still shown, and neither command
        /// is called eligible.
        #[tokio::test]
        async fn a_queue_that_cannot_be_read_is_said_and_recommends_nothing() {
            for phase in LOOKING {
                let world = world(true, false, true);
                let mut ledger = world.ledger();
                let message = world.message("review this");
                take_to(&mut ledger, &message, phase);
                let _asked = daemon(&world.binding.socket, world.agent.clone());
                let mut fake =
                    provider(|_, _| Err(json!({"code": -32000, "message": "queue unavailable"})))
                        .await;
                let before = world.stored();
                let shown = ask(&world, &mut ledger, &mut fake, None).await.unwrap();
                assert_eq!(world.stored(), before);
                let pending = &shown["pending"];
                assert_eq!(pending["phase"], phase.word());
                assert_eq!(pending["message"], message.id.as_str());
                assert!(pending["confirmation"].is_string());
                assert!(
                    pending["queue"]
                        .as_str()
                        .unwrap()
                        .starts_with("not established: "),
                    "{pending}"
                );
                for command in ["confirm_read", "start_queued"] {
                    assert!(
                        !pending["next"][command]
                            .as_str()
                            .unwrap()
                            .starts_with("eligible"),
                        "{phase:?} {command}"
                    );
                }
            }
        }

        /// Where the review says a command is refused, the command is
        /// refused in those words: by the phase, by the binding, and by a
        /// matching entry still scheduled. Nothing is written by a
        /// refusal, and the daemon is asked for no journal note and no
        /// acknowledgement.
        #[tokio::test]
        async fn a_command_is_refused_in_the_words_the_review_gave() {
            for phase in PHASES {
                if phase == Phase::Empty {
                    continue;
                }
                for shared in [false, true] {
                    if shared && testing::steered(phase) {
                        continue;
                    }
                    let world = world(shared, testing::steered(phase), true);
                    let mut ledger = world.ledger();
                    let message = world.message("review this");
                    take_to(&mut ledger, &message, phase);
                    let asked = daemon(&world.binding.socket, world.agent.clone());
                    // Something matching is still scheduled, edited.
                    let mut fake =
                        codex(vec![queue_entry("seen-now", message.id.as_str(), "edited")]).await;
                    let shown = ask(&world, &mut ledger, &mut fake, None).await.unwrap();
                    let next = shown["pending"]["next"].clone();
                    let before = world.stored();

                    let confirmation = shown["pending"]["confirmation"].as_str().unwrap();
                    if let Some(words) = next["confirm_read"]
                        .as_str()
                        .unwrap()
                        .strip_prefix("refused: ")
                    {
                        // A readback already begun is found by its
                        // confirmation and resumed, where a scheduled
                        // entry stops it in the same words.
                        let refused = ask(
                            &world,
                            &mut ledger,
                            &mut fake,
                            Some(("read", &message, confirmation)),
                        )
                        .await
                        .unwrap_err();
                        assert_eq!(refused.to_string(), words, "{phase:?}");
                    }
                    if let Some(words) = next["start_queued"]
                        .as_str()
                        .unwrap()
                        .strip_prefix("refused: ")
                    {
                        let digest = shown["pending"]["start_confirmation"]
                            .as_str()
                            .unwrap_or("0000");
                        let refused = ask(
                            &world,
                            &mut ledger,
                            &mut fake,
                            Some(("start", &message, digest)),
                        )
                        .await
                        .unwrap_err();
                        // Past the phase and the binding, an eligible start
                        // goes on to its own live checks, which this test
                        // does not stage: only what the review can foresee
                        // from the record is compared here.
                        if words != queued_start::ENTRY_UNAVAILABLE {
                            assert_eq!(refused.to_string(), words, "{phase:?} shared={shared}");
                        }
                    }
                    assert_eq!(
                        world.stored(),
                        before,
                        "a refusal writes nothing: {phase:?}"
                    );
                    assert!(
                        kinds(&asked).iter().all(|kind| *kind == "inspect"),
                        "{phase:?}: {:?}",
                        kinds(&asked)
                    );
                    assert!(
                        !fake.methods().iter().any(|m| m == "thread/queue/start"),
                        "{phase:?}"
                    );
                }
            }
        }

        /// A readback goes through once. Asked again with the same message
        /// and confirmation it answers that it was already applied, with
        /// another input pending by then: it reads nothing of the
        /// provider, journals nothing, acknowledges nothing and writes
        /// nothing.
        #[tokio::test]
        async fn a_finished_readback_asked_again_answers_already_applied() {
            let world = world(false, false, true);
            let mut ledger = world.ledger();
            let message = world.message("read this back");
            take_to(&mut ledger, &message, Phase::HookOffered);
            let asked = daemon(&world.binding.socket, world.agent.clone());
            let mut fake = codex(Vec::new()).await;
            let shown = ask(&world, &mut ledger, &mut fake, None).await.unwrap();
            assert!(
                shown["pending"]["next"]["confirm_read"]
                    .as_str()
                    .unwrap()
                    .starts_with("eligible")
            );
            let confirmation = shown["pending"]["confirmation"]
                .as_str()
                .unwrap()
                .to_owned();
            let done = ask(
                &world,
                &mut ledger,
                &mut fake,
                Some(("read", &message, &confirmation)),
            )
            .await
            .unwrap();
            assert_eq!(done["already_applied"], false);
            assert_eq!(ledger.phase(), Phase::Empty);
            assert!(
                ledger.latest_receipt().is_none(),
                "a readback is no receipt"
            );
            assert_eq!(
                kinds(&asked)
                    .iter()
                    .filter(|kind| **kind != "inspect")
                    .collect::<Vec<_>>(),
                [&"journal", &"inbox"]
            );

            // Another input is pending by the time it is asked again.
            let later = world.message("the next one");
            ledger.prepare(&later, None).unwrap();
            assert_eq!(ledger.phase(), Phase::Prepared);
            let (before, asked_before, read_before) =
                (world.stored(), kinds(&asked).len(), fake.methods().len());
            let again = ask(
                &world,
                &mut ledger,
                &mut fake,
                Some(("read", &message, &confirmation)),
            )
            .await
            .unwrap();
            assert_eq!(again["already_applied"], true);
            assert_eq!(again["resolution"], done["resolution"]);
            assert_eq!(world.stored(), before);
            assert_eq!(kinds(&asked)[asked_before..], ["inspect"]);
            assert_eq!(fake.methods().len(), read_before);
            assert_eq!(ledger.phase(), Phase::Prepared);
        }

        /// A readback that was begun and not finished is resumed by the
        /// same message and confirmation: its note is journaled once, so
        /// one already journaled only acknowledges.
        #[tokio::test]
        async fn a_readback_begun_is_resumed_from_where_it_stopped() {
            for journaled in [false, true] {
                let world = world(false, false, true);
                let mut ledger = world.ledger();
                let message = world.message("read this back");
                take_to(&mut ledger, &message, Phase::ManualRead { journaled });
                let read = ledger.pending_manual_read().unwrap().clone();
                let asked = daemon(&world.binding.socket, world.agent.clone());
                let mut fake = codex(Vec::new()).await;
                let done = ask(
                    &world,
                    &mut ledger,
                    &mut fake,
                    Some(("read", &message, &read.confirmation)),
                )
                .await
                .unwrap();
                assert_eq!(done["resolution"], read.id.as_str());
                assert_eq!(done["already_applied"], false);
                assert_eq!(ledger.phase(), Phase::Empty);
                let wrote: Vec<_> = kinds(&asked)
                    .into_iter()
                    .filter(|kind| *kind != "inspect")
                    .collect();
                assert_eq!(
                    wrote,
                    if journaled {
                        vec!["inbox"]
                    } else {
                        vec!["journal", "inbox"]
                    }
                );
            }
        }

        /// A start that may have been transmitted only reports when asked
        /// again, a receipt recorded beside it included: the phase reads
        /// Received then, and that must not take the answer away. Nothing
        /// is started, journaled, acknowledged or written.
        #[tokio::test]
        async fn a_transmitted_start_asked_again_only_reports() {
            for received in [false, true] {
                let world = world(true, false, true);
                let mut ledger = world.ledger();
                let message = world.message("start this");
                take_to(
                    &mut ledger,
                    &message,
                    Phase::StartTransmitted { replied: false },
                );
                if received {
                    ledger.received(testing::receipt("turn")).unwrap();
                    assert_eq!(ledger.phase(), Phase::Received);
                }
                let intent = ledger
                    .record()
                    .attempt
                    .as_ref()
                    .unwrap()
                    .start
                    .clone()
                    .unwrap();
                let asked = daemon(&world.binding.socket, world.agent.clone());
                let mut fake = codex(Vec::new()).await;
                let before = world.stored();
                let answer = ask(
                    &world,
                    &mut ledger,
                    &mut fake,
                    Some(("start", &message, &intent.confirmation)),
                )
                .await
                .unwrap();
                assert_eq!(answer["already_attempted"], true, "received={received}");
                assert_eq!(answer["queued_start"], intent.id.as_str());
                assert_eq!(world.stored(), before);
                assert_eq!(kinds(&asked), ["inspect"]);
                assert!(fake.methods().is_empty());
            }
        }

        /// Eligible is not promised. The review calls a start eligible
        /// from the record and its look, and says the command repeats its
        /// own checks; the command then refuses on one of them (here the
        /// shared server's record, which cannot be verified), having
        /// written no intent and started nothing.
        #[tokio::test]
        async fn an_eligible_start_is_still_the_commands_to_refuse() {
            let world = world(true, false, true);
            let mut ledger = world.ledger();
            let message = world.message("start this");
            take_to(&mut ledger, &message, Phase::Queued);
            let asked = daemon(&world.binding.socket, world.agent.clone());
            let text = ledger::input(&message).unwrap();
            let mut fake = codex(vec![queue_entry(
                "native-entry",
                message.id.as_str(),
                &text,
            )])
            .await;
            let shown = ask(&world, &mut ledger, &mut fake, None).await.unwrap();
            let pending = &shown["pending"];
            assert_eq!(pending["queue"], "the recorded entry, unchanged");
            let said = pending["next"]["start_queued"].as_str().unwrap();
            assert!(
                said.starts_with("eligible")
                    && said.contains("the command repeats every live check itself and may refuse"),
                "{said}"
            );
            let before = world.stored();
            let digest = pending["start_confirmation"].as_str().unwrap();
            let refused = ask(
                &world,
                &mut ledger,
                &mut fake,
                Some(("start", &message, digest)),
            )
            .await
            .unwrap_err();
            assert!(!refused.to_string().is_empty());
            assert_eq!(world.stored(), before, "no intent was written");
            assert_eq!(ledger.phase(), Phase::Queued);
            assert!(!fake.methods().iter().any(|m| m == "thread/queue/start"));
            assert!(kinds(&asked).iter().all(|kind| *kind == "inspect"));
        }
    }
}
