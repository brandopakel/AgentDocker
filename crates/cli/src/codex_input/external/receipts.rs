//! Read-only receipts from the existing provider conversation. Never resume it.
use super::{
    super::{ledger::Receipt, recovery, transport::Provider},
    ledger::Attempt,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::HashSet;

pub(super) async fn latest_item(provider: &mut Provider, thread: &str) -> Result<Option<String>> {
    let value = provider
        .request(
            "thread/items/list",
            json!({"threadId":thread,"limit":1,"sortDirection":"desc"}),
        )
        .await?;
    let items = value["data"]
        .as_array()
        .context("Codex history has no items")?;
    ensure!(items.len() <= 1, "Codex history exceeded requested limit");
    items
        .first()
        .map(|entry| {
            entry["item"]["id"]
                .as_str()
                .map(str::to_owned)
                .context("Codex history item has no ID")
        })
        .transpose()
}

pub(super) async fn find(
    provider: &mut Provider,
    thread: &str,
    attempt: &Attempt,
) -> Result<Option<Receipt>> {
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let mut cursor: Option<String> = None;
        let mut cursors = HashSet::new();
        let mut found = None;
        for _ in 0..100 {
            let value = provider
                .request(
                    "thread/items/list",
                    json!({"threadId":thread,"limit":50,"sortDirection":"desc","cursor":cursor}),
                )
                .await?;
            let (items, next) = recovery::page(&value, &mut cursors)?;
            for entry in items {
                if entry["item"]["id"]
                    .as_str()
                    .is_some_and(|id| attempt.anchor.as_deref() == Some(id))
                {
                    return Ok(found);
                }
                if let Some(receipt) = recovery::receipt(
                    thread,
                    entry["turnId"].as_str().unwrap_or_default(),
                    &entry["item"],
                    &attempt.input,
                )? {
                    ensure!(
                        found.is_none(),
                        "multiple provider receipts match one native queue input"
                    );
                    found = Some(receipt);
                }
            }
            cursor = next;
            if cursor.is_none() {
                return Ok(found);
            }
        }
        bail!("native queue receipt history exceeds the recovery limit")
    })
    .await
    .context("native queue receipt lookup exceeded one minute")?
}

pub(super) fn queued_id(value: &Value, attempt: &Attempt) -> Result<Option<String>> {
    let input = value["input"].as_array();
    if value["clientUserMessageId"].as_str() != Some(&attempt.message)
        || !input.is_some_and(|items| {
            items.len() == 1
                && items[0]["type"] == "text"
                && items[0]["text"].as_str() == Some(&attempt.input)
                && items[0]
                    .get("text_elements")
                    .is_none_or(|v| v.as_array().is_some_and(Vec::is_empty))
        })
    {
        return Ok(None);
    }
    let id = value["id"]
        .as_str()
        .context("native queued submission has no ID")?;
    ensure!(
        !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control),
        "invalid native queue ID"
    );
    Ok(Some(id.into()))
}

pub(super) async fn queued(
    provider: &mut Provider,
    thread: &str,
    attempt: &Attempt,
) -> Result<Option<String>> {
    let mut cursor: Option<String> = None;
    let mut cursors = HashSet::new();
    let mut found = None;
    for _ in 0..100 {
        let value = provider
            .request(
                "thread/queue/list",
                json!({"threadId":thread,"limit":50,"cursor":cursor}),
            )
            .await?;
        let (items, next) = recovery::page(&value, &mut cursors)?;
        for item in items {
            if let Some(id) = queued_id(item, attempt)? {
                ensure!(
                    found.is_none(),
                    "multiple native queue entries match one input"
                );
                found = Some(id);
            }
        }
        cursor = next;
        if cursor.is_none() {
            return Ok(found);
        }
    }
    bail!("native provider queue exceeds the recovery limit")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queue_acceptance_matches_the_entire_input_and_client_id() {
        let attempt = Attempt {
            message: "original".into(),
            input: "full input".into(),
            queued: None,
            receipt: None,
            anchor: None,
        };
        let mut value = json!({"id":"provider-queue","clientUserMessageId":"original","input":[{"type":"text","text":"full input","text_elements":[]}]});
        assert_eq!(
            queued_id(&value, &attempt).unwrap().as_deref(),
            Some("provider-queue")
        );
        value["input"][0]["text"] = json!("partial");
        assert!(queued_id(&value, &attempt).unwrap().is_none());
        value["input"][0]["text"] = json!("full input");
        value["clientUserMessageId"] = json!("another");
        assert!(queued_id(&value, &attempt).unwrap().is_none());
    }
}
