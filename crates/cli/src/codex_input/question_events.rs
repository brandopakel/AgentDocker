//! Subscribe before publishing questions. Reconnect only through checked replay;
//! a reply_to alone is never approval authority. Mutation failures still pause.
use super::Client;
use agentdocker_core::{EventCursor, EventKind, Response};
use anyhow::{Context, Result, anyhow, ensure};
use std::time::{Duration, Instant};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::timeout,
};

const BUFFER: usize = 128;
#[derive(Clone, Copy)]
struct Recovery {
    retries: usize,
    backoff: Duration,
    stable: Duration,
}
impl Default for Recovery {
    fn default() -> Self {
        Self {
            retries: 3,
            backoff: Duration::from_millis(100),
            stable: Duration::from_secs(30),
        }
    }
}

pub(super) struct Events {
    receiver: mpsc::Receiver<Result<EventKind>>,
    task: JoinHandle<()>,
}
impl Drop for Events {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Events {
    pub async fn start(client: Client) -> Result<Self> {
        Self::start_with(client, Recovery::default()).await
    }
    async fn start_with(client: Client, recovery: Recovery) -> Result<Self> {
        let (sender, receiver) = mpsc::channel(BUFFER);
        let (ready, received) = oneshot::channel();
        let task = tokio::spawn(worker(
            client.with_start_timeout(None),
            sender,
            ready,
            recovery,
        ));
        let events = Self { receiver, task };
        timeout(Duration::from_secs(5), received)
            .await
            .context("daemon question subscription timed out")?
            .context("daemon question subscription failed")??;
        Ok(events)
    }
    pub async fn next(&mut self) -> Result<EventKind> {
        self.receiver
            .recv()
            .await
            .context("daemon question event worker stopped")?
    }
}

fn relevant(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::QuestionClosed { .. } | EventKind::QuestionCancelled { .. }
    )
}

fn recoverable(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<tokio::time::error::Elapsed>()
            .is_some()
            || cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
                matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound
                        | std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::UnexpectedEof
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                )
            })
    })
}

async fn worker(
    client: Client,
    sender: mpsc::Sender<Result<EventKind>>,
    ready: oneshot::Sender<Result<()>>,
    policy: Recovery,
) {
    let mut ready = Some(ready);
    let mut committed: Option<EventCursor> = None;
    let mut failures = 0;
    let error = loop {
        let mut caught_up = None;
        let mut pending = Vec::new();
        let result = client
            .checked_events(committed.clone(), |response| {
                match response {
                    Response::EventsReadyAt { .. } => (),
                    Response::EventAt { cursor, event } => {
                        if caught_up.is_some() {
                            if relevant(&event.kind) {
                                sender.try_send(Ok(event.kind)).map_err(|_| {
                                    anyhow!(
                                        "provider question event buffer filled; delivery must pause"
                                    )
                                })?;
                            }
                            committed = Some(cursor);
                        } else if relevant(&event.kind) {
                            ensure!(
                                pending.len() < BUFFER,
                                "provider question replay buffer filled; delivery must pause"
                            );
                            pending.push(event.kind);
                        }
                    }
                    Response::EventsCaughtUp { cursor } => {
                        // Check capacity before publishing any of this replay. The
                        // worker is the only producer; the consumer only frees it.
                        ensure!(
                            sender.capacity() >= pending.len(),
                            "provider question event buffer filled; delivery must pause"
                        );
                        for event in pending.drain(..) {
                            sender
                                .try_send(Ok(event))
                                .map_err(|_| anyhow!("provider question event consumer stopped"))?;
                        }
                        committed = Some(cursor);
                        caught_up = Some(Instant::now());
                        if let Some(ready) = ready.take() {
                            let _ = ready.send(Ok(()));
                        }
                    }
                    _ => unreachable!("checked client validates every frame"),
                }
                Ok(true)
            })
            .await;
        let error = result
            .err()
            .unwrap_or_else(|| anyhow!("daemon question event stream ended"));
        if committed.is_none() || !recoverable(&error) {
            break error;
        }
        if caught_up.is_some_and(|at| at.elapsed() >= policy.stable) {
            failures = 0;
        }
        if failures >= policy.retries {
            break error
                .context("daemon question event reconnection exhausted; delivery must pause");
        }
        failures += 1;
        // Incomplete replay stays private and does not advance committed. A
        // reconnect must verify it again from the last delivered cursor.
        tokio::time::sleep(policy.backoff).await;
    };
    if let Some(ready) = ready {
        let _ = ready.send(Err(error));
    } else {
        let _ = sender.send(Err(error)).await;
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use agentdocker_core::{Event, Request};
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::{UnixListener, UnixStream},
    };
    type Peer = BufReader<UnixStream>;

    fn cursor(seq: u64) -> EventCursor {
        EventCursor {
            log: "a".repeat(32),
            seq,
            digest: if seq == 0 {
                String::new()
            } else {
                "b".repeat(64)
            },
        }
    }
    fn event(seq: u64) -> Response {
        Response::EventAt {
            cursor: cursor(seq),
            event: Event {
                seq,
                at: chrono::Utc::now(),
                kind: EventKind::QuestionClosed {
                    question: seq.to_string().into(),
                    answer: Some(format!("answer-{seq}").into()),
                },
            },
        }
    }
    async fn accept(listener: &UnixListener, after: Option<EventCursor>) -> Peer {
        let (stream, _) = timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut peer = BufReader::new(stream);
        let mut line = String::new();
        timeout(Duration::from_secs(2), peer.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(serde_json::from_str::<Request>(&line).unwrap(), Request::ResumeEvents { after: actual } if actual == after)
        );
        peer
    }
    async fn write(peer: &mut Peer, frame: Response) {
        peer.get_mut()
            .write_all((serde_json::to_string(&frame).unwrap() + "\n").as_bytes())
            .await
            .unwrap();
    }
    async fn ready(peer: &mut Peer, seq: u64) {
        write(
            peer,
            Response::EventsReadyAt {
                cursor: cursor(seq),
            },
        )
        .await;
        write(
            peer,
            Response::EventsCaughtUp {
                cursor: cursor(seq),
            },
        )
        .await;
    }
    async fn question(events: &mut Events, expected: u64) {
        let kind = timeout(Duration::from_secs(2), events.next())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(kind, EventKind::QuestionClosed { question, .. } if question.as_str()==expected.to_string())
        );
    }
    fn fast() -> Recovery {
        Recovery {
            retries: 1,
            backoff: Duration::from_millis(1),
            ..Recovery::default()
        }
    }

    #[tokio::test]
    async fn readiness_and_replayed_approvals_wait_for_complete_checked_history() {
        let home = tempfile::tempdir().unwrap();
        let socket = home.path().join("sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (subscribed, seen) = oneshot::channel();
        let (start, started) = oneshot::channel();
        let (reconnecting, reconnected) = oneshot::channel();
        let (finish, finished) = oneshot::channel();
        let server = tokio::spawn(async move {
            let mut peer = accept(&listener, None).await;
            write(&mut peer, Response::EventsReadyAt { cursor: cursor(0) }).await;
            subscribed.send(()).unwrap();
            started.await.unwrap();
            write(&mut peer, Response::EventsCaughtUp { cursor: cursor(0) }).await;
            write(&mut peer, event(1)).await;
            drop(peer);
            let mut peer = accept(&listener, Some(cursor(1))).await;
            write(&mut peer, Response::EventsReadyAt { cursor: cursor(1) }).await;
            write(&mut peer, event(2)).await;
            drop(peer); // replay has no completion marker
            let mut peer = accept(&listener, Some(cursor(1))).await; // not cursor 2
            reconnecting.send(()).unwrap();
            finished.await.unwrap();
            write(&mut peer, Response::EventsReadyAt { cursor: cursor(1) }).await;
            write(&mut peer, event(2)).await;
            write(&mut peer, Response::EventsCaughtUp { cursor: cursor(2) }).await;
            write(&mut peer, event(3)).await;
            let mut line = String::new();
            assert_eq!(
                timeout(Duration::from_secs(2), peer.read_line(&mut line))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
        });
        let starting = tokio::spawn(Events::start(Client::new(Some(socket))));
        seen.await.unwrap();
        assert!(!starting.is_finished());
        start.send(()).unwrap();
        let mut events = starting.await.unwrap().unwrap();
        question(&mut events, 1).await;
        reconnected.await.unwrap();
        assert!(
            timeout(Duration::from_millis(50), events.next())
                .await
                .is_err(),
            "incomplete replay cannot authorize a response"
        );
        finish.send(()).unwrap();
        question(&mut events, 2).await;
        question(&mut events, 3).await;
        drop(events);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn missing_history_is_not_retried_as_a_fresh_subscription() {
        let home = tempfile::tempdir().unwrap();
        let socket = home.path().join("sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let mut peer = accept(&listener, None).await;
            ready(&mut peer, 0).await;
            drop(peer);
            let mut peer = accept(&listener, Some(cursor(0))).await;
            write(
                &mut peer,
                Response::error(
                    agentdocker_core::ErrorCode::EventHistoryLost,
                    "retained anchor missing",
                ),
            )
            .await;
            drop(peer);
            assert!(
                timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
        });
        let mut events = Events::start_with(Client::new(Some(socket)), fast())
            .await
            .unwrap();
        assert!(
            events
                .next()
                .await
                .unwrap_err()
                .to_string()
                .contains("retained anchor missing")
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn repeated_transport_failure_is_bounded_and_old_readiness_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let socket = home.path().join("sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            for after in [None, Some(cursor(0))] {
                let mut peer = accept(&listener, after).await;
                ready(&mut peer, 0).await;
            }
        });
        let mut events = Events::start_with(Client::new(Some(socket)), fast())
            .await
            .unwrap();
        assert!(
            events
                .next()
                .await
                .unwrap_err()
                .to_string()
                .contains("reconnection exhausted")
        );
        server.await.unwrap();
        let socket = home.path().join("legacy");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let mut peer = accept(&listener, None).await;
            write(&mut peer, Response::EventsReady).await;
        });
        let error = match Events::start(Client::new(Some(socket))).await {
            Err(error) => error,
            Ok(_) => panic!("legacy stream admitted"),
        };
        assert!(error.to_string().contains("checked event readiness"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn excessive_question_replay_pauses_without_publishing_a_partial_batch() {
        let home = tempfile::tempdir().unwrap();
        let socket = home.path().join("sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let mut peer = accept(&listener, None).await;
            ready(&mut peer, 0).await;
            drop(peer);
            let mut peer = accept(&listener, Some(cursor(0))).await;
            write(&mut peer, Response::EventsReadyAt { cursor: cursor(0) }).await;
            for seq in 1..=BUFFER as u64 + 1 {
                write(&mut peer, event(seq)).await;
            }
            // No replayed approval may be exposed before completion.
        });
        let mut events = Events::start_with(Client::new(Some(socket)), fast())
            .await
            .unwrap();
        assert!(
            events
                .next()
                .await
                .unwrap_err()
                .to_string()
                .contains("replay buffer filled")
        );
        server.await.unwrap();
    }
}
