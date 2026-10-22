//! The receiver's way into a conversation that runs in the profile's shared
//! app-server (Codex 0.162 and later): its control socket, found and checked
//! again at every connection, since a restarted server may have moved it.
use super::{super::transport::Provider, ledger::Binding};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{collections::HashSet, path::Path};

const PAGE: usize = 256;
const MAX_PAGES: usize = 64;

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

/// The shared server has the conversation loaded: the one place its turns
/// run, so the one place its native queue is read. A conversation it does
/// not host is never delivered to through it.
pub(super) async fn hosts(provider: &mut Provider, thread: &str) -> Result<()> {
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
            return Ok(());
        }
        cursor = match page.get("nextCursor") {
            None | Some(Value::Null) => {
                bail!("the shared Codex server has not loaded this conversation")
            }
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
}
