//! What the receiver's tests share: a ledger taken to each phase, and
//! stand-ins for its two peers (a Codex app-server on a control socket, and
//! the daemon) for tests that go through its real handlers. Each stand-in
//! records what it was asked, in order, so a test can say what a command
//! did not do as well as what it did.
use super::super::ledger::Receipt;
use super::ledger::{self, Ledger, Phase};
use agentdocker_core::{Envelope, ProcessIdentity};

/// Every phase, the two markers in each of their values.
pub(super) const PHASES: [Phase; 12] = [
    Phase::Empty,
    Phase::Prepared,
    Phase::Queued,
    Phase::HookOffered,
    Phase::StartPrepared,
    Phase::StartTransmitted { replied: false },
    Phase::StartTransmitted { replied: true },
    Phase::SteerOffered,
    Phase::SteerAccepted,
    Phase::Received,
    Phase::ManualRead { journaled: false },
    Phase::ManualRead { journaled: true },
];

pub(super) fn operator() -> ProcessIdentity {
    ProcessIdentity {
        pid: 2,
        started_at: chrono::Utc::now(),
    }
}

pub(super) fn receipt(turn: &str) -> Receipt {
    Receipt {
        thread: "thread".into(),
        turn: turn.into(),
        item: "item".into(),
    }
}

/// Whether the phase is reached only by steering, which only a control
/// binding may do.
pub(super) fn steered(phase: Phase) -> bool {
    matches!(phase, Phase::SteerOffered | Phase::SteerAccepted)
}

/// Take `ledger`, which holds nothing, to `phase` by the calls that reach
/// it in the receiver.
pub(super) fn take_to(ledger: &mut Ledger, message: &Envelope, phase: Phase) {
    match phase {
        Phase::Empty => return,
        Phase::SteerOffered | Phase::SteerAccepted => {
            ledger.prepare_steer(message, None, "turn-a").unwrap();
        }
        _ => ledger.prepare(message, None).unwrap(),
    }
    match phase {
        Phase::Empty | Phase::Prepared | Phase::SteerOffered => {}
        Phase::Queued => ledger.queued("native-entry").unwrap(),
        Phase::HookOffered => ledger
            .offer_hook("hook-request", ledger::input(message).unwrap(), None)
            .unwrap(),
        Phase::StartPrepared | Phase::StartTransmitted { .. } => {
            ledger.queued("native-entry").unwrap();
            let confirmation = ledger.start_confirmation().unwrap();
            let start = ledger
                .begin_queued_start(message.id.as_str(), &confirmation, "a note", operator())
                .unwrap();
            if let Phase::StartTransmitted { replied } = phase {
                ledger.queued_start_transmitting(&start.id).unwrap();
                if replied {
                    ledger.queued_start_replied(&start.id, "turn").unwrap();
                }
            }
        }
        Phase::SteerAccepted => ledger.steer_accepted().unwrap(),
        Phase::Received => ledger.received(receipt("turn")).unwrap(),
        Phase::ManualRead { journaled } => {
            ledger
                .offer_hook("hook-request", ledger::input(message).unwrap(), None)
                .unwrap();
            let confirmation = ledger.confirmation().unwrap();
            ledger
                .begin_manual_read(message.id.as_str(), &confirmation, "a note", operator())
                .unwrap();
            if journaled {
                let id = ledger.pending_manual_read().unwrap().id.clone();
                ledger.manual_journaled(&id).unwrap();
            }
        }
    }
}

#[cfg(unix)]
pub(super) use peers::*;

#[cfg(unix)]
mod peers {
    use super::super::{
        Provider,
        ledger::{self, Binding, Ledger},
    };
    use agentdocker_core::{
        AgentRecord, AgentSpec, AgentStatus, Envelope, ProcessIdentity, ProjectRef,
        ProviderGeneration, Request, Response,
    };
    use agentdocker_host::{ipc, procinfo};
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use std::{
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
    };
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio_tungstenite::tungstenite::Message;

    /// A folder short enough to hold a socket on every platform.
    pub(in crate::codex_input::external) fn short_root() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("cq-")
            .tempdir_in(if cfg!(target_os = "macos") {
                PathBuf::from("/tmp")
            } else {
                std::env::temp_dir()
            })
            .unwrap()
    }

    /// The requests a stand-in was sent: for the provider its method and
    /// parameters, for the daemon the request itself.
    pub(in crate::codex_input::external) type Seen<T> = Arc<Mutex<Vec<T>>>;

    pub(in crate::codex_input::external) struct FakeProvider {
        _root: tempfile::TempDir,
        pub provider: Provider,
        pub seen: Seen<(String, Value)>,
    }

    impl FakeProvider {
        /// The methods asked so far, in order.
        pub fn methods(&self) -> Vec<String> {
            self.seen
                .lock()
                .unwrap()
                .iter()
                .map(|(method, _)| method.clone())
                .collect()
        }
    }

    /// A provider whose server answers each request with what `answer` gives
    /// for its method and parameters: a result, or an error object.
    pub(in crate::codex_input::external) async fn provider(
        mut answer: impl FnMut(&str, &Value) -> Result<Value, Value> + Send + 'static,
    ) -> FakeProvider {
        let root = short_root();
        let path = root.path().join("control.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let seen: Seen<(String, Value)> = Arc::default();
        let log = seen.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(text))) = socket.next().await {
                let request: Value = serde_json::from_str(&text).unwrap();
                let method = request["method"].as_str().unwrap().to_owned();
                log.lock()
                    .unwrap()
                    .push((method.clone(), request["params"].clone()));
                let reply = match answer(&method, &request["params"]) {
                    Ok(result) => json!({"id": request["id"], "result": result}),
                    Err(error) => json!({"id": request["id"], "error": error}),
                };
                if socket.send(Message::text(reply.to_string())).await.is_err() {
                    break;
                }
            }
        });
        let provider = Provider::connect_control(&path).await.unwrap();
        FakeProvider {
            _root: root,
            provider,
            seen,
        }
    }

    /// One page of the native queue holding `entries`, and no more pages.
    pub(in crate::codex_input::external) fn queue_page(entries: &[Value]) -> Value {
        json!({"data": entries, "nextCursor": null})
    }

    /// A native queue entry as Codex lists it.
    pub(in crate::codex_input::external) fn queue_entry(
        id: &str,
        message: &str,
        text: &str,
    ) -> Value {
        json!({
            "id": id,
            "clientUserMessageId": message,
            "input": [{"type": "text", "text": text}],
        })
    }

    /// A receiver's world for one test: a home, the live conversation's
    /// record as the daemon holds it, and the binding that names it. The
    /// provider process is this test's own, so it is alive and its start time
    /// is exact.
    pub(in crate::codex_input::external) struct World {
        _root: tempfile::TempDir,
        pub home: PathBuf,
        pub agent: AgentRecord,
        pub binding: Binding,
    }

    pub(in crate::codex_input::external) fn world(
        shared: bool,
        control: bool,
        project: bool,
    ) -> World {
        let root = short_root();
        let home = root.path().canonicalize().unwrap();
        let pid = std::process::id();
        let started_at = procinfo::start_time(pid).unwrap();
        let mut agent = AgentRecord::new(
            AgentSpec {
                name: "codex-1".into(),
                runtime: "codex".into(),
                workdir: Some(home.clone()),
                labels: [("session_id".to_owned(), "thread".to_owned())].into(),
                ..Default::default()
            },
            false,
            chrono::Utc::now(),
        );
        agent.status = AgentStatus::Running;
        agent.pid = Some(pid);
        agent.process_started_at = Some(started_at);
        agent.project = project.then(|| ProjectRef::directory(home.clone()));
        let binding = Binding {
            agent: agent.id.to_string(),
            provider: ProviderGeneration {
                process: ProcessIdentity { pid, started_at },
                session: "thread".into(),
                profile: home.to_string_lossy().into_owned(),
            },
            socket: home.join("sock"),
            cwd: home.clone(),
            executable: home.join("codex"),
            remote: shared.then(|| super::super::remote::Descriptor {
                record: home.join("server.json"),
                sha256: "a".repeat(64),
            }),
            control,
        };
        World {
            _root: root,
            home,
            agent,
            binding,
        }
    }

    impl World {
        pub fn ledger(&self) -> Ledger {
            Ledger::open(&self.home, self.binding.clone(), None).unwrap()
        }

        pub fn message(&self, text: &str) -> Envelope {
            Envelope::new(
                "peer",
                agentdocker_core::Destination::parse(&self.binding.agent),
                "chat",
                json!({ "text": text }),
                None,
                chrono::Utc::now(),
            )
        }

        /// The ledger's file, as it is on disk now.
        pub fn stored(&self) -> Vec<u8> {
            std::fs::read(
                ledger::directory(&self.home, &self.binding.agent)
                    .unwrap()
                    .join("delivery.json"),
            )
            .unwrap()
        }
    }

    /// A daemon on `socket` that knows `agent` and answers what a recovery
    /// command may ask of it. It records every request; a test that expects
    /// no journal note and no acknowledgement reads that record.
    pub(in crate::codex_input::external) fn daemon(
        socket: &Path,
        agent: AgentRecord,
    ) -> Seen<Request> {
        let listener = ipc::Listener::bind(socket).unwrap();
        let seen: Seen<Request> = Arc::default();
        let log = seen.clone();
        tokio::spawn(async move {
            let mut journal = 0_u64;
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
                    continue;
                }
                let request: Request = serde_json::from_str(&line).unwrap();
                let response = match &request {
                    Request::Inspect { .. } => Response::Agent {
                        agent: agent.clone(),
                    },
                    Request::JournalAdd { agent, summary } => {
                        journal += 1;
                        Response::JournalEntry {
                            entry: serde_json::from_value(json!({
                                "project": "project", "seq": journal, "at": chrono::Utc::now(),
                                "agent": agent, "agent_name": "codex-1", "kind": "note",
                                "summary": summary, "summary_source": "explicit"
                            }))
                            .unwrap(),
                        }
                    }
                    Request::ProviderInbox { agent, .. } => Response::InputBatch {
                        agent: agent.clone().into(),
                        messages: vec![],
                        uncertain: vec![],
                        answers_routed: true,
                    },
                    other => panic!("a recovery command asked the daemon for {other:?}"),
                };
                log.lock().unwrap().push(request);
                let mut line = serde_json::to_vec(&response).unwrap();
                line.push(b'\n');
                let _ = stream.get_mut().write_all(&line).await;
            }
        });
        seen
    }
}
