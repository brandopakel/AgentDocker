//! Retained queue reads can be repeated; uncertain acknowledgements cannot.
use crate::client::Client;
use agentdocker_core::{MessageId, Request, Response};
use anyhow::Result;
use std::time::Duration;

pub(super) fn recoverable(error: &anyhow::Error) -> bool {
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

pub(super) async fn queue(
    client: &Client,
    agent: &str,
    acknowledge: Vec<MessageId>,
) -> Result<Response> {
    let read_only = acknowledge.is_empty();
    let request = Request::ProviderInbox {
        agent: agent.into(),
        acknowledge,
    };
    if !read_only {
        // Even a lost response may follow an accepted acknowledgement.
        return super::call(client, request).await;
    }
    let client = client.clone().with_start_timeout(None);
    for attempt in 0..=3 {
        match super::call(&client, request.clone()).await {
            Err(error) if attempt < 3 && recoverable(&error) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            result => return result,
        }
    }
    unreachable!("the final queue-read attempt always returns")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::UnixListener,
    };

    #[tokio::test]
    async fn a_lost_read_reply_repeats_only_the_empty_ack_queue_request() {
        let home = tempfile::tempdir().unwrap();
        let socket = home.path().join("queue.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            for index in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                assert!(
                    matches!(serde_json::from_str::<Request>(&line).unwrap(), Request::ProviderInbox { agent, acknowledge } if agent == "owned" && acknowledge.is_empty())
                );
                if index == 1 {
                    reader
                        .get_mut()
                        .write_all(b"{\"type\":\"messages\",\"messages\":[]}\n")
                        .await
                        .unwrap();
                }
            }
        });
        let client = Client::new(Some(socket));
        assert!(matches!(
            queue(&client, "owned", vec![]).await.unwrap(),
            Response::Messages { .. }
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn reads_stop_after_three_retries_but_uncertain_acks_and_protocol_errors_stop_once() {
        for case in 0..3 {
            let home = tempfile::tempdir().unwrap();
            let socket = home.path().join("queue.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let expected = if case == 0 { 4 } else { 1 };
            let server = tokio::spawn(async move {
                for _ in 0..expected {
                    let (stream, _) = listener.accept().await.unwrap();
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    reader.read_line(&mut line).await.unwrap();
                    let Request::ProviderInbox { acknowledge, .. } =
                        serde_json::from_str::<Request>(&line).unwrap()
                    else {
                        panic!("unexpected request")
                    };
                    assert_eq!(acknowledge.len(), usize::from(case == 1));
                    if case == 2 {
                        reader
                            .get_mut()
                            .write_all(b"{\"type\":\"unknown-response\"}\n")
                            .await
                            .unwrap();
                    }
                }
                assert!(
                    tokio::time::timeout(Duration::from_millis(200), listener.accept())
                        .await
                        .is_err(),
                    "request was retried after its boundary"
                );
            });
            let client = Client::new(Some(socket)).with_start_timeout(None);
            let acknowledge = if case == 1 {
                vec!["received".to_owned().into()]
            } else {
                vec![]
            };
            assert!(queue(&client, "owned", acknowledge).await.is_err());
            server.await.unwrap();
        }
    }
}
