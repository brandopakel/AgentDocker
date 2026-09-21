//! Explicit recovery for an offered hook whose native receipt cannot be proven.
//! Reading history never silently authorizes this. A caller reviews the retained
//! input, then confirms its generation-bound digest; the sole receiver journals
//! that manual disposition and acknowledges only that message, without a receipt.
use super::{Client, Ledger, Provider, call, identity, ledger, receipts, verify_provider};
use agentdocker_core::{ProcessIdentity, ProviderGeneration, Request, Response};
use agentdocker_host::{dirs, procinfo};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::PathBuf,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
};

#[derive(clap::Args)]
pub struct Args {
    /// Exact Codex agent whose retained input should be reviewed.
    #[arg(long, env = "AGENTDOCKER_AGENT_ID")]
    pub agent: String,
    /// Message ID from the review. Omit all three options for a read-only preview.
    #[arg(long, requires_all = ["confirm_read", "note"])]
    pub message: Option<String>,
    /// Confirm that you read the complete message, using the preview's digest.
    #[arg(long, requires_all = ["message", "note"])]
    pub confirm_read: Option<String>,
    /// Why manual readback resolves this message (recorded in the journal).
    #[arg(long, requires_all = ["message", "confirm_read"])]
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
}

pub(super) struct Listener {
    socket: UnixListener,
    path: PathBuf,
}

impl Listener {
    pub fn bind(home: &std::path::Path, agent: &str) -> Result<Self> {
        let path = ledger::directory(home, agent)?.join("resolve.sock");
        match std::fs::symlink_metadata(&path) {
            Ok(meta) => {
                ensure!(
                    meta.file_type().is_socket() && meta.uid() == unsafe { libc::geteuid() },
                    "unsafe input recovery endpoint"
                );
                std::fs::remove_file(&path)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        let socket = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self { socket, path })
    }

    pub async fn accept(&self) -> std::io::Result<UnixStream> {
        self.socket.accept().await.map(|r| r.0)
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

async fn frame(stream: &mut UnixStream, max: usize) -> Result<Vec<u8>> {
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

async fn write(stream: &mut UnixStream, response: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(response)?;
    bytes.push(b'\n');
    stream.write_all(&bytes).await?;
    Ok(())
}

fn preview(ledger: &Ledger) -> Result<Value> {
    let record = ledger.record();
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
            }))
        })
        .transpose()?;
    Ok(
        json!({"agent":record.binding.agent,"provider":record.binding.provider,"pending":pending,
        "notice":"Review the complete envelope before confirming. Confirmation records manual readback, not automatic provider delivery. It never replays the input."}),
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
        "matching native input is still queued; manual acknowledgement refused"
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
        Command::Preview { .. } => preview(ledger),
        Command::Confirm {
            message,
            confirmation,
            note,
            ..
        } => {
            ensure!(
                current.project.is_some(),
                "manual readback requires a project journal; retained input is unchanged"
            );
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
                ensure!(
                    receipts::queued_for_manual(
                        provider,
                        &ledger.record().binding.provider.session,
                        attempt
                    )
                    .await?
                    .is_none(),
                    "matching native input is still queued; manual acknowledgement refused"
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
    mut stream: UnixStream,
    client: &Client,
    provider: &mut Provider,
    ledger: &mut Ledger,
) -> Result<()> {
    let result = tokio::time::timeout(Duration::from_secs(75), async {
        let peer = stream.peer_cred()?;
        // This is an explicit local administration command. Same-user callers
        // may act from their shell or the app; they need not be provider children.
        ensure!(
            peer.uid() == unsafe { libc::geteuid() },
            "input recovery belongs to another user"
        );
        let pid = peer
            .pid()
            .and_then(|p| u32::try_from(p).ok())
            .context("input recovery peer identity unavailable")?;
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
    tokio::select! {
        stream = listener.accept() => {
            let stream = stream?;
            let binding = ledger.record().binding.clone();
            let mut provider = Provider::start_profile(&binding.executable, &[], &binding.cwd, Some(std::path::Path::new(&binding.provider.profile)))?;
            let result = async {
                verify_provider(&mut provider, &binding).await?;
                serve(stream, client, &mut provider, ledger).await
            }.await;
            let shutdown = provider.shutdown().await;
            result.and(shutdown)
        }
        _ = tokio::time::sleep(Duration::from_secs(30)) => Ok(()),
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
    let path = ledger::directory(&dirs::home(), agent.id.as_str())?.join("resolve.sock");
    let mut stream = UnixStream::connect(path).await.context(
        "receiver has no recovery endpoint; upgrade the receiver before resolving retained input",
    )?;
    let accepted = agent
        .input_binding
        .as_ref()
        .context("receiver binding disappeared")?;
    let peer = stream.peer_cred()?;
    ensure!(
        peer.uid() == unsafe { libc::geteuid() }
            && peer.pid().and_then(|p| u32::try_from(p).ok()) == Some(accepted.controller.pid)
            && procinfo::start_time(accepted.controller.pid)
                == Some(accepted.controller.started_at),
        "recovery endpoint is not owned by the bound receiver"
    );
    let command = match (args.message, args.confirm_read, args.note) {
        (Some(message), Some(confirmation), Some(note)) => Command::Confirm {
            agent: agent.id.to_string(),
            provider: accepted.provider.clone(),
            socket: client.socket_path().into(),
            message,
            confirmation,
            note,
        },
        (None, None, None) => Command::Preview {
            agent: agent.id.to_string(),
            provider: accepted.provider.clone(),
            socket: client.socket_path().into(),
        },
        _ => bail!("confirming readback requires --message, --confirm-read and --note together"),
    };
    tokio::time::timeout(Duration::from_secs(90), async {
        write(&mut stream, &serde_json::to_value(command)?).await?;
        let response: Value = serde_json::from_slice(&frame(&mut stream, 2 * 1024 * 1024).await?)?;
        if let Some(error) = response["error"].as_str() {
            bail!("{error}");
        }
        let value = response.get("ok").context("invalid recovery response")?;
        println!("{}", serde_json::to_string_pretty(value)?);
        Ok(())
    })
    .await
    .context("receiver did not answer; input and any pending disposition are retained")?
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{Destination, Envelope};

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
        let listener = UnixListener::bind(&socket).unwrap();
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
}
