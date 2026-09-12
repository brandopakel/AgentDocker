//! Verify the complete replay boundary before a consumer treats reconnect as
//! ready. A caller retains cursors only after processing their response.
use super::*;
use agentdocker_core::{EventCursor, event::EVENT_STREAM_FRAME_BYTES};
use tokio::{
    io::AsyncReadExt,
    time::{Instant as TokioInstant, timeout, timeout_at},
};

const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

impl Client {
    /// One checked connection, with no implicit fresh subscription or retry.
    /// EOF is a failure: resume explicitly using the last processed cursor.
    pub async fn checked_events(
        &self,
        after: Option<EventCursor>,
        mut on_response: impl FnMut(Response) -> Result<bool>,
    ) -> Result<()> {
        if after.as_ref().is_some_and(|c| !c.is_valid()) {
            bail!("invalid event cursor");
        }
        let deadline = TokioInstant::now() + FRAME_TIMEOUT;
        let mut reader = timeout_at(
            deadline,
            self.connect(&Request::ResumeEvents {
                after: after.clone(),
            }),
        )
        .await
        .context("event subscription connection timed out")??;
        let first = timeout_at(deadline, read_frame(&mut reader))
            .await
            .context("event subscription readiness timed out")??;
        let Response::EventsReadyAt {
            cursor: mut position,
        } = first
        else {
            bail!("daemon did not acknowledge checked event readiness; update the daemon");
        };
        anyhow::ensure!(
            position.is_valid() && after.as_ref().is_none_or(|c| *c == position),
            "daemon changed the requested event cursor"
        );
        if !on_response(Response::EventsReadyAt {
            cursor: position.clone(),
        })? {
            return Ok(());
        }
        let mut caught_up = false;
        loop {
            // Replay must finish within one deadline. An idle live stream can
            // wait indefinitely, but an unfinished frame has a bounded lifetime.
            let response = if caught_up {
                read_frame(&mut reader).await?
            } else {
                timeout_at(deadline, read_frame(&mut reader))
                    .await
                    .context("event replay completion timed out")??
            };
            match &response {
                Response::EventAt { cursor, event } => {
                    anyhow::ensure!(
                        event.kind != agentdocker_core::EventKind::Unknown,
                        "unknown event kind in checked stream; update the client before resuming"
                    );
                    anyhow::ensure!(
                        cursor.is_valid()
                            && cursor.log == position.log
                            && cursor.seq == position.seq + 1
                            && event.seq == cursor.seq,
                        "daemon event stream is not contiguous"
                    );
                    position = cursor.clone();
                }
                Response::EventsCaughtUp { cursor } if !caught_up && *cursor == position => {
                    caught_up = true
                }
                _ => bail!("unexpected response in checked event stream"),
            }
            if !on_response(response)? {
                return Ok(());
            }
        }
    }
}

async fn read_frame(reader: &mut BufReader<Stream>) -> Result<Response> {
    // Wait for the first byte without imposing a deadline on idle sessions.
    if reader.fill_buf().await?.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "checked event stream closed; resume from the last processed cursor",
        )
        .into());
    }
    let mut line = String::new();
    let count = timeout(
        FRAME_TIMEOUT,
        reader
            .take(EVENT_STREAM_FRAME_BYTES as u64 + 1)
            .read_line(&mut line),
    )
    .await
    .context("checked event frame timed out")??;
    anyhow::ensure!(
        count <= EVENT_STREAM_FRAME_BYTES,
        "checked event frame is oversized or incomplete"
    );
    if !line.ends_with('\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "checked event frame is oversized or incomplete",
        )
        .into());
    }
    into_result(serde_json::from_str(&line)?)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use agentdocker_core::{Event, EventKind};

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
                kind: EventKind::WatcherStarted,
            },
        }
    }

    async fn consume(
        frames: Vec<Response>,
        after: Option<EventCursor>,
    ) -> (Result<()>, Vec<Response>) {
        let home = tempfile::tempdir().unwrap();
        let socket = home.path().join("sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let expected = after.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut request = String::new();
            reader.read_line(&mut request).await.unwrap();
            assert!(
                matches!(serde_json::from_str::<Request>(&request).unwrap(), Request::ResumeEvents { after } if after == expected)
            );
            for frame in frames {
                let data = serde_json::to_string(&frame).unwrap() + "\n";
                if reader.get_mut().write_all(data.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        let mut seen = Vec::new();
        let result = Client::new(Some(socket))
            .with_start_timeout(None)
            .checked_events(after, |response| {
                seen.push(response);
                Ok(true)
            })
            .await;
        server.await.unwrap();
        (result, seen)
    }

    #[tokio::test]
    async fn checked_replay_then_live_retains_the_exact_processed_cursors() {
        let (result, seen) = consume(
            vec![
                Response::EventsReadyAt { cursor: cursor(2) },
                event(3),
                event(4),
                Response::EventsCaughtUp { cursor: cursor(4) },
                event(5),
            ],
            Some(cursor(2)),
        )
        .await;
        assert!(result.unwrap_err().to_string().contains("closed"));
        assert_eq!(seen.len(), 5);
        assert!(matches!(&seen[3], Response::EventsCaughtUp { cursor } if cursor.seq == 4));
        assert!(matches!(&seen[4], Response::EventAt { cursor, .. } if cursor.seq == 5));
    }

    #[tokio::test]
    async fn invalid_boundaries_do_not_reach_the_consumer() {
        let cases = vec![
            (vec![Response::EventsReady], 0),
            (vec![Response::EventsReadyAt { cursor: cursor(1) }], 0),
            (
                vec![Response::EventsReadyAt { cursor: cursor(2) }, event(4)],
                1,
            ),
            (
                vec![Response::EventsReadyAt { cursor: cursor(2) }, event(2)],
                1,
            ),
            (
                vec![
                    Response::EventsReadyAt { cursor: cursor(2) },
                    Response::EventsCaughtUp { cursor: cursor(3) },
                ],
                1,
            ),
            (
                vec![
                    Response::EventsReadyAt { cursor: cursor(2) },
                    Response::EventsCaughtUp { cursor: cursor(2) },
                    Response::EventsCaughtUp { cursor: cursor(2) },
                ],
                2,
            ),
            (
                vec![
                    Response::EventsReadyAt { cursor: cursor(2) },
                    Response::EventsReadyAt { cursor: cursor(2) },
                ],
                1,
            ),
            (
                vec![Response::EventsReadyAt { cursor: cursor(2) }, Response::End],
                1,
            ),
            (
                vec![Response::error(
                    agentdocker_core::ErrorCode::EventHistoryLost,
                    "retention gap",
                )],
                0,
            ),
        ];
        for (frames, valid_count) in cases {
            let (result, seen) = consume(frames, Some(cursor(2))).await;
            assert!(result.is_err());
            assert_eq!(seen.len(), valid_count);
        }
        let mut mismatch = event(3);
        if let Response::EventAt { event, .. } = &mut mismatch {
            event.seq = 4;
        }
        let (_, seen) = consume(
            vec![Response::EventsReadyAt { cursor: cursor(2) }, mismatch],
            Some(cursor(2)),
        )
        .await;
        assert_eq!(seen.len(), 1);
        let unknown = Response::EventAt {
            cursor: cursor(3),
            event: Event {
                seq: 3,
                at: chrono::Utc::now(),
                kind: EventKind::Unknown,
            },
        };
        let (result, seen) = consume(
            vec![Response::EventsReadyAt { cursor: cursor(2) }, unknown],
            Some(cursor(2)),
        )
        .await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unknown event kind")
        );
        assert_eq!(seen.len(), 1);
    }

    #[tokio::test]
    async fn checked_frames_reject_partial_oversized_and_trickled_input() {
        for bytes in [
            b"{\"type\":\"ok\"}".to_vec(),
            vec![b' '; EVENT_STREAM_FRAME_BYTES + 1],
        ] {
            let (client, mut server) = agentdocker_host::ipc::pair().await.unwrap();
            let writer = tokio::spawn(async move {
                let _ = server.write_all(&bytes).await;
            });
            let mut reader = BufReader::new(client);
            assert!(
                read_frame(&mut reader)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("oversized or incomplete")
            );
            drop(reader);
            writer.await.unwrap();
        }
        let (client, mut server) = agentdocker_host::ipc::pair().await.unwrap();
        server.write_all(b"{").await.unwrap();
        let mut reader = BufReader::new(client);
        assert!(
            read_frame(&mut reader)
                .await
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
    }

    #[tokio::test]
    async fn checked_frames_preserve_split_utf8() {
        let (client, mut server) = agentdocker_host::ipc::pair().await.unwrap();
        let writer = tokio::spawn(async move {
            let data = "{\"type\":\"error\",\"code\":\"event_history_lost\",\"message\":\"é\"}\n"
                .as_bytes();
            let split = data.iter().position(|b| *b == 0xc3).unwrap() + 1;
            server.write_all(&data[..split]).await.unwrap();
            tokio::task::yield_now().await;
            server.write_all(&data[split..]).await.unwrap();
        });
        assert!(
            read_frame(&mut BufReader::new(client))
                .await
                .unwrap_err()
                .to_string()
                .contains('é')
        );
        writer.await.unwrap();
    }
}
