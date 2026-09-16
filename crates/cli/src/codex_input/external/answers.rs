//! Distinguish an MCP tool-result answer from an asynchronous question's input.
use super::super::{
    ledger::Receipt,
    mcp_answers::{self, Origin},
    recovery,
    transport::Provider,
};
use agentdocker_core::Envelope;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::HashSet;

pub(super) enum Route {
    Input,
    Received(Receipt),
    Waiting,
}

pub(super) async fn origin(provider: &mut Provider, human: String) -> Result<Origin> {
    let value = provider
        .request("config/read", json!({"includeLayers":false}))
        .await?;
    // Read configuration metadata only; do not alter it or log its contents.
    let servers = value["config"]["mcp_servers"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, server)| {
            server["enabled"] != false
                && server.get("url").is_none_or(Value::is_null)
                && server["command"].as_str().is_some_and(|p| {
                    std::path::Path::new(p)
                        .file_name()
                        .is_some_and(|n| n == "agentdocker")
                })
                && server["args"]
                    .as_array()
                    .is_some_and(|args| args.iter().any(|v| v == "mcp"))
        })
        .map(|(name, _)| name.clone())
        .collect();
    let origin = Origin { human, servers };
    origin.validate()?;
    Ok(origin)
}

fn returned(item: &Value, origin: &Origin) -> Option<Value> {
    if item["type"] != "mcpToolCall"
        || item["tool"] != "ask_human"
        || item["status"] != "completed"
        || !item["error"].is_null()
        || !origin
            .servers
            .iter()
            .any(|s| item["server"].as_str() == Some(s))
    {
        return None;
    }
    let content = item["result"]["content"]
        .as_array()
        .filter(|c| c.len() == 1)?;
    if content[0]["type"] != "text" {
        return None;
    }
    serde_json::from_str(content[0]["text"].as_str()?).ok()
}

pub(super) fn classify(
    origin: &Origin,
    thread: &str,
    entry: &Value,
    envelope: &Envelope,
    agent: &str,
) -> Result<Route> {
    let Some(value) = returned(&entry["item"], origin) else {
        return Ok(Route::Waiting);
    };
    if value["answered"] == true && value["message_id"].as_str() == Some(envelope.id.as_str()) {
        let proof = mcp_answers::proof(
            origin,
            thread,
            entry["turnId"].as_str().unwrap_or_default(),
            &entry["item"],
            std::slice::from_ref(envelope),
            agent,
        )?
        .context("MCP answer has no exact provider receipt")?;
        return Ok(Route::Received(proof.receipt));
    }
    if value["posted"] == true
        && value["answer_delivery"] == "native_queue"
        && envelope
            .reply_to
            .as_ref()
            .is_some_and(|id| value["question_id"].as_str() == Some(id.as_str()))
    {
        return Ok(Route::Input);
    }
    Ok(Route::Waiting)
}

pub(super) async fn route(
    provider: &mut Provider,
    origin: &Origin,
    thread: &str,
    envelope: &Envelope,
    agent: &str,
    queue_route: bool,
) -> Result<Route> {
    if queue_route || envelope.kind != "answer" || envelope.from.as_str() != origin.human {
        return Ok(Route::Input);
    }
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let mut cursor: Option<String> = None;
        let mut cursors = HashSet::new();
        for _ in 0..100 {
            let value = provider
                .request(
                    "thread/items/list",
                    json!({"threadId":thread,"limit":50,"sortDirection":"desc","cursor":cursor}),
                )
                .await?;
            let (items, next) = recovery::page(&value, &mut cursors)?;
            for entry in items {
                match classify(origin, thread, entry, envelope, agent)? {
                    Route::Waiting => (),
                    route => return Ok(route),
                }
            }
            cursor = next;
            if cursor.is_none() {
                return Ok(Route::Waiting);
            }
        }
        bail!("question receipt history exceeds the recovery limit")
    })
    .await
    .context("question receipt lookup exceeded one minute")?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn synchronous_answers_are_receipts_and_only_exact_async_questions_become_input() {
        let origin = Origin {
            human: "person".into(),
            servers: vec!["coordination".into()],
        };
        let envelope = Envelope::new(
            "person",
            agentdocker_core::Destination::parse("agent"),
            "answer",
            json!({"text":"Blue"}),
            Some("question".to_owned().into()),
            chrono::Utc::now(),
        );
        let item = |value: Value| json!({"turnId":"turn","item":{"type":"mcpToolCall","tool":"ask_human","server":"coordination","status":"completed","id":"item","error":null,"result":{"content":[{"type":"text","text":value.to_string()}]}}});
        let value =
            item(json!({"answered":true,"message_id":envelope.id,"from":"person","text":"Blue"}));
        assert!(matches!(
            classify(&origin, "thread", &value, &envelope, "agent").unwrap(),
            Route::Received(_)
        ));
        let value =
            item(json!({"posted":true,"question_id":"question","answer_delivery":"native_queue"}));
        assert!(matches!(
            classify(&origin, "thread", &value, &envelope, "agent").unwrap(),
            Route::Input
        ));
        let value =
            item(json!({"posted":true,"question_id":"another","answer_delivery":"native_queue"}));
        assert!(matches!(
            classify(&origin, "thread", &value, &envelope, "agent").unwrap(),
            Route::Waiting
        ));
        let mut value =
            item(json!({"answered":true,"message_id":envelope.id,"from":"person","text":"Blue"}));
        value["item"]["server"] = json!("unrelated");
        assert!(matches!(
            classify(&origin, "thread", &value, &envelope, "agent").unwrap(),
            Route::Waiting
        ));
    }
}
