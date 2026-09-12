//! Subscribe before publishing any provider questions. Missing closure events
//! pauses delivery; an arbitrary reply_to is never itself approval authority.
use super::Client;
use agentdocker_core::{EventKind, Request, Response};
use anyhow::{Context, Result, anyhow};
use std::time::Duration;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::timeout,
};

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
        let (sender, receiver) = mpsc::channel(128);
        let (ready, received) = oneshot::channel();
        let task = tokio::spawn(async move {
            let result = client
                .stream_after(
                    &Request::Events {
                        replay: 0,
                        ready: true,
                    },
                    async move {
                        let _ = ready.send(());
                        Ok(())
                    },
                    |(), response| {
                        if let Response::Event { event } = response
                            && let kind @ (EventKind::QuestionClosed { .. }
                            | EventKind::QuestionCancelled { .. }) = event.kind
                        {
                            sender.try_send(Ok(kind)).map_err(|_| {
                                anyhow!(
                                    "provider question event buffer filled; delivery must pause"
                                )
                            })?;
                        }
                        Ok(true)
                    },
                )
                .await;
            let error = result
                .err()
                .unwrap_or_else(|| anyhow!("daemon question event stream closed"));
            let _ = sender.send(Err(error)).await;
        });
        let events = Self { receiver, task };
        timeout(Duration::from_secs(5), received)
            .await
            .context("daemon question subscription timed out")?
            .context("daemon question subscription failed")?;
        Ok(events)
    }

    pub async fn next(&mut self) -> Result<EventKind> {
        self.receiver
            .recv()
            .await
            .context("daemon question event worker stopped")?
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn readiness_precedes_question_work_and_event_loss_is_an_error() {
        let home = tempfile::tempdir().unwrap();
        let socket = home.path().join("sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let (seen, read) = oneshot::channel();
        let (release, proceed) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            assert!(matches!(
                serde_json::from_str::<Request>(&line).unwrap(),
                Request::Events {
                    replay: 0,
                    ready: true
                }
            ));
            seen.send(()).unwrap();
            proceed.await.unwrap();
            let event = agentdocker_core::Event::new(
                EventKind::QuestionClosed {
                    question: "question".to_owned().into(),
                    answer: Some("answer".to_owned().into()),
                },
                chrono::Utc::now(),
            );
            let lines = [
                serde_json::to_string(&Response::EventsReady).unwrap(),
                serde_json::json!({"type":"event","event":event}).to_string(),
                serde_json::to_string(&Response::Lagged { skipped: 1 }).unwrap(),
            ]
            .join("\n")
                + "\n";
            reader.get_mut().write_all(lines.as_bytes()).await.unwrap();
        });
        let starting = tokio::spawn(Events::start(
            Client::new(Some(socket)).with_start_timeout(None),
        ));
        timeout(Duration::from_secs(5), read)
            .await
            .unwrap()
            .unwrap();
        assert!(!starting.is_finished());
        release.send(()).unwrap();
        let mut events = starting.await.unwrap().unwrap();
        assert!(
            matches!(events.next().await.unwrap(),EventKind::QuestionClosed { question,answer:Some(answer) } if question.as_str()=="question" && answer.as_str()=="answer")
        );
        assert!(
            events
                .next()
                .await
                .unwrap_err()
                .to_string()
                .contains("lost 1 events")
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_closed_event_stream_never_silently_reconnects_or_claims_readiness() {
        for ready in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let socket = home.path().join("sock");
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                if ready {
                    reader
                        .get_mut()
                        .write_all(
                            (serde_json::to_string(&Response::EventsReady).unwrap() + "\n")
                                .as_bytes(),
                        )
                        .await
                        .unwrap();
                }
            });
            let result = Events::start(Client::new(Some(socket)).with_start_timeout(None)).await;
            if ready {
                assert!(
                    result
                        .unwrap()
                        .next()
                        .await
                        .unwrap_err()
                        .to_string()
                        .contains("closed")
                );
            } else {
                assert!(result.is_err());
            }
            server.await.unwrap();
        }
    }
}
