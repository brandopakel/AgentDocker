//! Observe a newly owned server before its sole native terminal starts.
//! This connection cannot answer approvals or submit input.
use super::websocket::Remote;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::time::Duration;

pub(crate) struct BirthObserver {
    remote: Remote,
    sequence: u64,
    thread: Option<Value>,
}

impl BirthObserver {
    pub async fn connect(port: u16, token: &str) -> Result<Self> {
        let mut observer = Self {
            remote: Remote::connect(port, token).await?,
            sequence: 0,
            thread: None,
        };
        observer
            .request("initialize", json!({
                "clientInfo":{"name":"agentdocker_native_launcher","version":env!("CARGO_PKG_VERSION")},
                "capabilities":{"experimentalApi":true}
            }))
            .await?;
        observer
            .remote
            .send(serde_json::to_vec(&json!({"method":"initialized"}))?)
            .await?;
        let empty = observer.request("thread/loaded/list", json!({})).await?;
        ensure!(
            empty["data"].as_array().is_some_and(Vec::is_empty)
                && empty.get("nextCursor").is_some_and(Value::is_null)
                && observer.thread.is_none(),
            "owned native server is not empty before terminal startup"
        );
        Ok(observer)
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .context("startup request limit reached")?;
        let id = self.sequence;
        self.remote
            .send(serde_json::to_vec(
                &json!({"id":id,"method":method,"params":params}),
            )?)
            .await?;
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut bytes = 0;
            for _ in 0..128 {
                let value = self.remote.receive().await?;
                bytes += serde_json::to_vec(&value)?.len();
                ensure!(
                    bytes <= super::MAX_FRAME,
                    "native startup notification limit reached"
                );
                if value.get("method").is_none() && value["id"].as_u64() == Some(id) {
                    ensure!(
                        value.get("error").is_none(),
                        "native startup request was rejected"
                    );
                    return value
                        .get("result")
                        .cloned()
                        .context("native startup response has no result");
                }
                ensure!(
                    value.get("id").is_none(),
                    "native startup observer cannot answer provider requests"
                );
                match value["method"].as_str() {
                    Some("thread/started") => {
                        ensure!(
                            self.thread.is_none(),
                            "native startup observed multiple thread births"
                        );
                        self.thread = Some(value["params"]["thread"].clone());
                    }
                    Some("turn/started" | "item/started") => {
                        anyhow::bail!("native input began before startup binding")
                    }
                    Some(_) => (),
                    None => anyhow::bail!("invalid native startup notification"),
                }
            }
            anyhow::bail!("native startup notification count exceeded")
        })
        .await
        .context("native startup observation timed out")?
    }

    pub async fn observed(&mut self) -> Result<Option<Value>> {
        let loaded = self.request("thread/loaded/list", json!({})).await?;
        let ids = loaded["data"]
            .as_array()
            .context("native loaded threads unavailable")?;
        ensure!(
            ids.len() <= 1 && loaded.get("nextCursor").is_some_and(Value::is_null),
            "native startup thread is ambiguous"
        );
        match (&self.thread, ids.first()) {
            (Some(thread), Some(id)) => {
                ensure!(
                    thread["id"] == *id && id.is_string(),
                    "native birth differs from loaded thread"
                );
                Ok(Some(thread.clone()))
            }
            _ => Ok(None),
        }
    }

    /// Resume does not broadcast a new thread birth. An initially empty owned
    /// server may load only the explicit requested thread. This proves no fresh
    /// allowance: the launcher must still verify ordinary persisted history.
    pub async fn resumed(&mut self, session: &str) -> Result<Option<Value>> {
        let loaded = self.request("thread/loaded/list", json!({})).await?;
        let ids = loaded["data"]
            .as_array()
            .context("native resumed threads unavailable")?;
        ensure!(
            ids.len() <= 1 && loaded.get("nextCursor").is_some_and(Value::is_null),
            "native resumed thread is ambiguous"
        );
        let Some(id) = ids.first() else {
            return Ok(None);
        };
        ensure!(
            id.as_str() == Some(session)
                && self
                    .thread
                    .as_ref()
                    .is_none_or(|thread| thread["id"] == *id),
            "native terminal loaded another conversation"
        );
        let value = self
            .request(
                "thread/read",
                json!({"threadId":session,"includeTurns":false}),
            )
            .await?;
        ensure!(
            value["thread"]["id"].as_str() == Some(session),
            "native resumed metadata names another conversation"
        );
        Ok(Some(value["thread"].clone()))
    }

    pub async fn close(self) -> Result<()> {
        self.remote.close().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    async fn probe(events: Vec<Value>, ids: Value) -> Result<Option<Value>> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            for method in [
                "initialize",
                "initialized",
                "thread/loaded/list",
                "thread/loaded/list",
            ] {
                let value = socket.next().await.unwrap().unwrap();
                let value: Value =
                    serde_json::from_slice(value.into_text().unwrap().as_bytes()).unwrap();
                assert_eq!(value["method"], method);
                if method == "initialized" {
                    continue;
                }
                let id = value["id"].clone();
                let result = if method == "initialize" {
                    json!({})
                } else if id == 2 {
                    json!({"data":[],"nextCursor":null})
                } else {
                    for event in &events {
                        let _ = socket.send(Message::Text(event.to_string().into())).await;
                    }
                    json!({"data":ids,"nextCursor":null})
                };
                let _ = socket
                    .send(Message::Text(
                        json!({"id":id,"result":result}).to_string().into(),
                    ))
                    .await;
            }
        });
        let mut observer =
            BirthObserver::connect(port, "private-startup-capability-0123456789").await?;
        let result = observer.observed().await;
        server.await.unwrap();
        result
    }

    fn born(id: &str) -> Value {
        json!({"method":"thread/started","params":{"thread":{"id":id}}})
    }

    #[tokio::test]
    async fn observes_only_the_new_thread_of_an_initially_empty_server() {
        assert_eq!(
            probe(vec![born("new")], json!(["new"])).await.unwrap(),
            Some(json!({"id":"new"}))
        );
        assert_eq!(probe(vec![], json!([])).await.unwrap(), None);
        assert_eq!(probe(vec![], json!(["unwitnessed"])).await.unwrap(), None);
    }

    #[tokio::test]
    async fn startup_refuses_multiple_births_changed_threads_input_and_approvals() {
        for (events, ids) in [
            (vec![born("new"), born("new")], json!(["new"])),
            (vec![born("new")], json!(["other"])),
            (vec![born("new")], json!(["new", "other"])),
            (
                vec![json!({"method":"turn/started","params":{}})],
                json!([]),
            ),
            (
                vec![json!({"method":"item/started","params":{}})],
                json!([]),
            ),
            (
                vec![json!({"id":99,"method":"item/commandExecution/requestApproval"})],
                json!([]),
            ),
        ] {
            assert!(probe(events, ids).await.is_err());
        }
    }
}
