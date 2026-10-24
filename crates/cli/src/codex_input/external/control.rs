//! The receiver's way into a conversation that runs in the profile's shared
//! app-server (Codex 0.162 and later): its control socket, found and checked
//! again at every connection, since a restarted server may have moved it.
//! A message that arrives during a turn nobody waits on is steered into it
//! (`turn/steer` naming that turn); anything else goes to the native queue,
//! which the conversation takes at idle.
use super::{
    super::transport::{Provider, rejected},
    ledger::Binding,
};
use agentdocker_host::codex_control::ThreadStatus;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{collections::HashSet, path::Path};

const PAGE: usize = 256;
const MAX_PAGES: usize = 64;

/// What the shared server's conversation can take now, read from the
/// conversation itself immediately before an offer.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Moment {
    /// A turn is running and waits on nobody: steer into it, naming it.
    Steer(String),
    /// The running turn waits on the person, for an approval or an answer:
    /// nothing is offered until it stops waiting.
    Hold,
    /// No running turn this receiver can name, or a status it cannot read:
    /// the native queue, which the conversation takes at idle.
    Queue,
}

/// The newest turn, then the status: the status last, so it is as fresh
/// as it can be when the steer goes. The server checks the turn again
/// itself (`expectedTurnId`), so a turn that ended in between is refused,
/// never steered into.
pub(super) async fn moment(provider: &mut Provider, thread: &str) -> Result<Moment> {
    let running = newest_turn(provider, thread)
        .await?
        .filter(|(_, state)| state == "inProgress")
        .map(|(id, _)| id);
    Ok(match status(provider, thread).await? {
        Some(
            ThreadStatus::Active {
                waiting_on_approval: true,
                ..
            }
            | ThreadStatus::Active {
                waiting_on_user_input: true,
                ..
            },
        ) => Moment::Hold,
        Some(ThreadStatus::Active { .. }) => running.map_or(Moment::Queue, Moment::Steer),
        _ => Moment::Queue,
    })
}

/// Whether `turn`, which an input was steered into, may still take it: it is
/// the conversation's newest turn, and in progress or the conversation is
/// still active. Codex takes steered input at the turn's next step, so its
/// receipt can follow long after the steer was answered.
pub(super) async fn still_running(
    provider: &mut Provider,
    thread: &str,
    turn: &str,
) -> Result<bool> {
    let Some((newest, state)) = newest_turn(provider, thread).await? else {
        return Ok(false);
    };
    if newest != turn {
        return Ok(false);
    }
    Ok(state == "inProgress"
        || matches!(
            status(provider, thread).await?,
            Some(ThreadStatus::Active { .. })
        ))
}

/// The conversation's newest turn and its status, without items. A read the
/// server refuses names none; a failed connection is an error.
async fn newest_turn(provider: &mut Provider, thread: &str) -> Result<Option<(String, String)>> {
    let value = match provider
        .request(
            "thread/turns/list",
            json!({"threadId":thread,"limit":1,"itemsView":"notLoaded","sortDirection":"desc"}),
        )
        .await
    {
        Ok(value) => value,
        Err(error) if rejected(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    let turns = value["data"]
        .as_array()
        .context("thread/turns/list has no data")?;
    ensure!(turns.len() <= 1, "thread/turns/list exceeded its page size");
    let Some(turn) = turns.first() else {
        return Ok(None);
    };
    let id = turn["id"]
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control))
        .context("thread/turns/list named an invalid turn")?;
    let state = turn["status"]
        .as_str()
        .context("thread/turns/list gave a turn no status")?;
    Ok(Some((id.to_owned(), state.to_owned())))
}

/// The conversation's status from `thread/read`; `None` when the server
/// refuses the read or says something this build does not know.
async fn status(provider: &mut Provider, thread: &str) -> Result<Option<ThreadStatus>> {
    let value = match provider
        .request(
            "thread/read",
            json!({"threadId":thread,"includeTurns":false}),
        )
        .await
    {
        Ok(value) => value,
        Err(error) if rejected(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    ensure!(
        value["thread"]["id"].as_str() == Some(thread),
        "thread/read answered another conversation"
    );
    Ok(ThreadStatus::parse(&value["thread"]["status"]))
}

/// Connect to the shared server the binding's profile names now. The
/// initialized profile and the loaded conversation are checked after.
#[cfg(unix)]
pub(super) async fn connect(binding: &Binding) -> Result<Provider> {
    let socket = agentdocker_host::codex_control::locate(Path::new(&binding.provider.profile))
        .context("the Codex control socket cannot be trusted")?
        .context("the Codex profile exposes no control socket; its shared server is not running")?;
    Provider::connect_control(&socket).await
}

#[cfg(not(unix))]
pub(super) async fn connect(_binding: &Binding) -> Result<Provider> {
    bail!("Codex has no control socket on this platform")
}

/// Whether `profile` has a shared server a receiver could reach. A profile
/// without the socket, or with one this user cannot trust, has none.
pub(super) fn available(profile: &Path) -> bool {
    matches!(
        agentdocker_host::codex_control::locate(profile),
        Ok(Some(_))
    )
}

/// Why new input waits while the shared server has unloaded the conversation.
pub(super) const UNLOADED: &str = "The shared Codex server has not loaded this conversation; new messages wait until it is open again.";

/// The shared server has the conversation loaded: the one place its turns
/// run, so the one place its native queue is read. A conversation it does
/// not host is never delivered to through it.
pub(super) async fn hosts(provider: &mut Provider, thread: &str) -> Result<()> {
    ensure!(
        loaded(provider, thread).await?,
        "the shared Codex server has not loaded this conversation"
    );
    Ok(())
}

/// Whether the shared server has the conversation loaded now, from every
/// page of `thread/loaded/list`.
pub(super) async fn loaded(provider: &mut Provider, thread: &str) -> Result<bool> {
    let mut cursor: Option<String> = None;
    let mut cursors = HashSet::new();
    for _ in 0..MAX_PAGES {
        let page = provider
            .request(
                "thread/loaded/list",
                json!({"cursor": cursor, "limit": PAGE}),
            )
            .await?;
        let data = page["data"]
            .as_array()
            .context("thread/loaded/list has no data")?;
        ensure!(
            data.len() <= PAGE,
            "thread/loaded/list exceeded its page size"
        );
        if data.iter().any(|id| id.as_str() == Some(thread)) {
            return Ok(true);
        }
        cursor = match page.get("nextCursor") {
            None | Some(Value::Null) => return Ok(false),
            Some(Value::String(next))
                if !next.is_empty() && next.len() <= 4096 && cursors.insert(next.clone()) =>
            {
                Some(next.clone())
            }
            _ => bail!("thread/loaded/list returned an invalid cursor"),
        };
    }
    bail!("thread/loaded/list exceeded the page limit")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    /// `thread/loaded/list` pages: the first look finds the thread on its
    /// second page, the second look ends without it, the third repeats a
    /// cursor.
    #[tokio::test]
    async fn only_a_conversation_the_shared_server_has_loaded_is_reached_through_it() {
        let root = tempfile::Builder::new()
            .prefix("cs-")
            .tempdir_in(if cfg!(target_os = "macos") {
                std::path::PathBuf::from("/tmp")
            } else {
                std::env::temp_dir()
            })
            .unwrap();
        let path = root.path().join("control.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut look = 0;
            while let Some(Ok(Message::Text(text))) = socket.next().await {
                let request: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(request["method"], "thread/loaded/list");
                let cursor = request["params"]["cursor"].as_str();
                if cursor.is_none() {
                    look += 1;
                }
                let page = match (look, cursor) {
                    (_, None) => json!({"data": ["other"], "nextCursor": "next"}),
                    (1, Some("next")) => json!({"data": ["wanted"], "nextCursor": null}),
                    (2, Some("next")) => json!({"data": [], "nextCursor": null}),
                    (3, Some("next")) => json!({"data": [], "nextCursor": "next"}),
                    other => panic!("unexpected page {other:?}"),
                };
                socket
                    .send(Message::text(
                        json!({"id": request["id"], "result": page}).to_string(),
                    ))
                    .await
                    .unwrap();
            }
        });
        let mut provider = Provider::connect_control(&path).await.unwrap();
        hosts(&mut provider, "wanted").await.unwrap();
        assert!(
            hosts(&mut provider, "wanted")
                .await
                .unwrap_err()
                .to_string()
                .contains("has not loaded")
        );
        assert!(
            hosts(&mut provider, "wanted")
                .await
                .unwrap_err()
                .to_string()
                .contains("invalid cursor")
        );
        provider.shutdown().await.unwrap();
        server.await.unwrap();
    }

    #[test]
    fn a_profile_without_the_socket_has_no_shared_server() {
        let profile = tempfile::tempdir().unwrap();
        assert!(!available(profile.path()));
    }

    /// A conversation read in turns: each look answers `thread/turns/list`
    /// with the newest turn (or refuses it) and `thread/read` with a status
    /// (or refuses it), in that order, as the receiver asks them.
    #[tokio::test]
    async fn only_a_running_turn_nobody_waits_on_is_steered_into() {
        let root = tempfile::Builder::new()
            .prefix("cm-")
            .tempdir_in(if cfg!(target_os = "macos") {
                std::path::PathBuf::from("/tmp")
            } else {
                std::env::temp_dir()
            })
            .unwrap();
        let path = root.path().join("control.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let turn = |id: &str, status: &str| {
            Some(json!({"data": [{"id": id, "status": status,
            "items": [], "itemsView": "notLoaded"}], "nextCursor": null}))
        };
        let active = |flags: Value| Some(json!({"type": "active", "activeFlags": flags}));
        // (newest turn or a refusal, status or a refusal when it is asked)
        let looks: Vec<(Option<Value>, Option<Option<Value>>)> = vec![
            (turn("t1", "inProgress"), Some(active(json!([])))),
            (
                turn("t1", "inProgress"),
                Some(active(json!(["waitingOnApproval"]))),
            ),
            (
                turn("t1", "inProgress"),
                Some(active(json!(["waitingOnUserInput"]))),
            ),
            (turn("t1", "completed"), Some(Some(json!({"type": "idle"})))),
            // The list has not caught up with a turn the status says runs.
            (turn("t1", "completed"), Some(active(json!([])))),
            (None, Some(active(json!([])))),
            (turn("t1", "inProgress"), Some(None)),
            (
                turn("t1", "inProgress"),
                Some(Some(json!({"type": "later"}))),
            ),
            // still_running: the steered turn, another one, a turn the list
            // calls interrupted while the conversation runs, a finished one.
            (turn("t1", "inProgress"), None),
            (turn("t2", "inProgress"), None),
            (turn("t1", "interrupted"), Some(active(json!([])))),
            (turn("t1", "completed"), Some(Some(json!({"type": "idle"})))),
        ];
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut answers = std::collections::VecDeque::new();
            for (list, status) in looks {
                answers.push_back(("thread/turns/list", list));
                // A look the newest turn alone decides asks no status.
                if let Some(status) = status {
                    answers.push_back(("thread/read", status));
                }
            }
            while let Some(Ok(Message::Text(text))) = socket.next().await {
                let request: Value = serde_json::from_str(&text).unwrap();
                let (method, answer) = answers.pop_front().expect("an expected request");
                assert_eq!(request["method"], method, "{request}");
                let params = &request["params"];
                assert_eq!(params["threadId"], "thread");
                if method == "thread/turns/list" {
                    assert_eq!(params["limit"], 1);
                    assert_eq!(params["sortDirection"], "desc");
                    assert_eq!(params["itemsView"], "notLoaded");
                }
                let reply = match (method, answer) {
                    (_, None) => json!({"id": request["id"],
                        "error": {"code": -32601, "message": "not served"}}),
                    ("thread/read", Some(status)) => json!({"id": request["id"],
                        "result": {"thread": {"id": "thread", "status": status}}}),
                    (_, Some(list)) => json!({"id": request["id"], "result": list}),
                };
                socket.send(Message::text(reply.to_string())).await.unwrap();
            }
            assert!(answers.is_empty(), "every look was asked: {answers:?}");
        });
        let mut provider = Provider::connect_control(&path).await.unwrap();
        let mut moments = Vec::new();
        for _ in 0..8 {
            moments.push(moment(&mut provider, "thread").await.unwrap());
        }
        assert_eq!(
            moments,
            [
                Moment::Steer("t1".into()),
                Moment::Hold,
                Moment::Hold,
                Moment::Queue,
                Moment::Queue,
                Moment::Queue,
                Moment::Queue,
                Moment::Queue,
            ]
        );
        let mut running = Vec::new();
        for _ in 0..4 {
            running.push(still_running(&mut provider, "thread", "t1").await.unwrap());
        }
        assert_eq!(running, [true, false, true, false]);
        provider.shutdown().await.unwrap();
        server.await.unwrap();
    }
}
